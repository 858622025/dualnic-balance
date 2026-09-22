//! 对账自愈（写路由）：把路由表收敛到确定性期望态（单默认 + 内网前缀 + 固定 metric）。
//!
//! **这是全项目唯一会真写系统路由 / 改接口 metric / 禁用启用网卡的模块，需管理员权限。**
//! 红线：受保护接口（reconciliation.protected_interfaces 的 GUID）**绝不**删除/改 metric/禁用。
//!
//! 权限：写路由需管理员；LocalSystem 服务天然可写，console 需管理员（否则返回 5=ACCESS_DENIED）。
//! 字节序：读路径 `u32::from_be`；写路径 `u32::from(ip).to_be()`（相反）。

use std::net::Ipv4Addr;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};

use windows::Win32::Foundation::BOOLEAN;
use windows::Win32::NetworkManagement::IpHelper::{
    ConvertInterfaceIndexToLuid, CreateIpForwardEntry, DeleteIpForwardEntry,
    GetIpInterfaceEntry, InitializeIpInterfaceEntry, SetIpInterfaceEntry,
    MIB_IPFORWARDROW, MIB_IPINTERFACE_ROW, MIB_IPROUTE_METRIC_UNUSED,
    MIB_IPROUTE_TYPE_DIRECT, MIB_IPROUTE_TYPE_INDIRECT,
};
use windows::Win32::NetworkManagement::Ndis::NET_LUID_LH;
use windows::Win32::Networking::WinSock::{AF_INET, MIB_IPPROTO_NETMGMT};

use dualnic_core::diff::{diff_snapshot, DiffKind};
use dualnic_core::ip::{Prefix};
use dualnic_core::ipc::{ReconcileResult, ReconcileStep};
use dualnic_core::role::normalize_guid;

use crate::status::{config_snapshot, RuntimeState};

const ACCESS_DENIED: u32 = 5;
const ERROR_NOT_FOUND: u32 = 1168;
const ERROR_FILE_NOT_FOUND: u32 = 2;
/// CreateIpForwardEntry：路由已存在。不计入「新增」（否则每轮对账虚报「加 N 条」）。
const ERROR_OBJECT_ALREADY_EXISTS: u32 = 5010;

// ──────────────────────────── 入口 ────────────────────────────

/// 手动「一键收敛」：阻塞，route_guard 串行。
pub fn reconcile_once(st: &RuntimeState) -> ReconcileResult {
    let _guard = st.route_guard.lock().unwrap();
    if st.paused.load(Ordering::Acquire) {
        return paused_result();
    }
    let r = do_reconcile(st);
    log_reconcile(st, &r);
    r
}

/// 自动对账：try_lock，忙则跳过（不排队堆积）。
pub fn try_reconcile(st: &RuntimeState) -> Option<ReconcileResult> {
    let _guard = st.route_guard.try_lock().ok()?;
    if st.paused.load(Ordering::Acquire) {
        return Some(paused_result());
    }
    let r = do_reconcile(st);
    log_reconcile(st, &r);
    Some(r)
}

/// 当前方案「不管理路由」（对账开关全关）时，清掉本工具此前添加的前缀路由（manifest 记录）。
/// 只清前缀：默认路由不恢复（该方案明确不动默认路由）、metric 不动。manifest 清空后幂等。
pub fn cleanup_tool_routes_if_any(st: &RuntimeState) -> Option<ReconcileResult> {
    let _guard = st.route_guard.try_lock().ok()?;
    if st.paused.load(Ordering::Acquire) {
        return None;
    }
    let manifest = load_manifest();
    if manifest.added_routes.is_empty() {
        return None;
    }

    let mut result = ReconcileResult::empty();
    for key in &manifest.added_routes {
        let row = make_delete_row(key.dest, key.mask_len, key.gateway, key.if_index);
        match unsafe { DeleteIpForwardEntry(&row) } {
            0 | ERROR_NOT_FOUND | ERROR_FILE_NOT_FOUND => {
                result.removed_static_prefixes += 1;
                result.steps.push(ReconcileStep {
                    op: "delete_static_prefix".into(),
                    target: format!("{}/{}", key.dest, key.mask_len),
                    detail: dualnic_core::msgref!("DNLOG", 60; &key.if_index.to_string()),
                });
            }
            _ => {}
        }
    }
    let _ = std::fs::remove_file(crate::paths::route_manifest_path());
    if result.removed_static_prefixes > 0 {
        st.db.append_event_msg(
            "info",
            "reconcile",
            &dualnic_core::msgref!("DNREC", 12; &result.removed_static_prefixes),
        );
        log_reconcile(st, &result);
    }
    Some(result)
}

/// 把对账结果记进结构化事件日志（SQLite）。
fn log_reconcile(st: &RuntimeState, r: &ReconcileResult) {
    if r.removed_defaults + r.removed_static_prefixes + r.added_prefixes + r.metrics_fixed > 0 {
        st.db.append_event_msg(
            "info",
            "reconcile",
            &dualnic_core::msgref!(
                "DNREC", 10;
                &r.removed_defaults, &r.removed_static_prefixes, &r.added_prefixes, &r.metrics_fixed
            ),
        );
    } else if let Some(e) = &r.error {
        // 嵌套消息：服务端无语言运行时，arg 以代码形态存档（GUI 端极少出现该路径）
        st.db.append_event_msg("warn", "reconcile", &dualnic_core::msgref!("DNREC", 11; &e.code_form()));
    }
}

fn paused_result() -> ReconcileResult {
    ReconcileResult {
        ok: true,
        removed_defaults: 0,
        removed_static_prefixes: 0,
        added_prefixes: 0,
        metrics_fixed: 0,
        steps: vec![],
        error: Some(dualnic_core::msgref!("DNREC", 15;)),
    }
}

// ──────────────────────────── 执行器 ────────────────────────────

fn do_reconcile(st: &RuntimeState) -> ReconcileResult {
    let cfg = config_snapshot(st).config;
    if !cfg.reconciliation.converge_default_route && !cfg.reconciliation.remove_stale_defaults {
        return ReconcileResult::empty();
    }
    let adapters = match crate::net::adapters_views() {
        Ok(a) => a,
        Err(e) => return err_result(dualnic_core::msgref!("DNREC", 16; &dualnic_core::msg::t(&e))),
    };
    let routes = match crate::net::all_ipv4_routes() {
        Ok(r) => r,
        Err(e) => return err_result(dualnic_core::msgref!("DNREC", 17; &dualnic_core::msg::t(&e))),
    };
    let report = diff_snapshot(&cfg, &adapters, &routes);

    // 收集 recommended 且非 protected 的动作
    let protected: Vec<String> = cfg.reconciliation.protected_interfaces.iter().cloned().collect();
    let actions: Vec<_> = report
        .actions
        .iter()
        .filter(|a| a.recommended)
        .filter(|a| !is_protected(&protected, a.guid.as_deref()))
        .collect();

    let mut result = ReconcileResult::empty();
    let mut manifest = load_manifest();

    // ① 先固定接口 metric（WAN/LAN）。必须先于加前缀：Vista+ 起 dwForwardMetric1 = 路由 metric
    //    + 接口 metric，须 ≥ 接口 metric；先把接口 metric 落到期望值，再加前缀路由才合法。
    if let Some(wan) = &report.wan {
        // 受保护接口绝不改 metric（红线）。
        if !is_protected(&protected, wan.guid.as_deref()) {
            match set_interface_metric(wan.if_index, cfg.interface_metric.wan, false) {
                Ok(MetricOutcome::Written) => {
                    result.metrics_fixed += 1;
                    result.steps.push(ReconcileStep { op: "set_metric".into(), target: format!("WAN if#{}", wan.if_index), detail: dualnic_core::msgref!("DNLOG", 67; &cfg.interface_metric.wan.to_string()) });
                }
                Ok(MetricOutcome::Unchanged) => {}
                Ok(MetricOutcome::Failed(ACCESS_DENIED)) => return permission_result(),
                Ok(MetricOutcome::Failed(rc)) => tracing::warn!(if_index = wan.if_index, rc, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 40;))),
                Err(e) => tracing::warn!(if_index = wan.if_index, error = %e, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 41;))),
            }
        }
    }
    if let Some(lan) = &report.lan {
        // 受保护接口绝不改 metric（红线）。
        if !is_protected(&protected, lan.guid.as_deref()) {
            match set_interface_metric(lan.if_index, cfg.interface_metric.lan, false) {
                Ok(MetricOutcome::Written) => {
                    result.metrics_fixed += 1;
                    result.steps.push(ReconcileStep { op: "set_metric".into(), target: format!("LAN if#{}", lan.if_index), detail: dualnic_core::msgref!("DNLOG", 67; &cfg.interface_metric.lan.to_string()) });
                }
                Ok(MetricOutcome::Unchanged) => {}
                Ok(MetricOutcome::Failed(ACCESS_DENIED)) => return permission_result(),
                Ok(MetricOutcome::Failed(rc)) => tracing::warn!(if_index = lan.if_index, rc, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 42;))),
                Err(e) => tracing::warn!(if_index = lan.if_index, error = %e, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 43;))),
            }
        }
    }

    // ② 默认路由收敛（converge_default_route 开启）：
    //    - 删掉所有「非 WAN 卡」上的默认路由（LAN/多余接口）。路由判定用 forward_type/next_hop，
    //      默认路由本身 mask_len=0，一定是「带网关」那一类，不会有直连误删问题。
    //    - 确保「WAN 卡」上存在默认路由：读 WAN 卡当前 DHCP 网关作为下一跳写入。（网关值来自
    //      DHCP，程序不钦定网关地址；谁当 WAN 由配置决定。）若 WAN 网关缺失（无 DHCP 网关）则只删不补。
    if cfg.reconciliation.converge_default_route {
        // 先删非 WAN 默认
        for r in routes.iter().filter(|r| r.is_default()) {
            let on_wan = report.wan.as_ref().is_some_and(|c| r.if_index == c.if_index);
            if on_wan {
                continue;
            }
            let Some(view) = adapters.iter().find(|a| a.if_index == r.if_index) else {
                // 未识别接口（不在网卡枚举里，如多 compartment/未枚举虚拟卡）：不认识 → 一律保留，绝不删。
                tracing::warn!(if_index = r.if_index, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 44;)));
                continue;
            };
            if is_protected(&protected, view.guid.as_deref()) {
                continue; // 受保护接口的默认路由：保留+告警，绝不删
            }
            let row = make_delete_row(r.dest, 0, r.gateway, r.if_index);
            match unsafe { DeleteIpForwardEntry(&row) } {
                0 | ERROR_NOT_FOUND | ERROR_FILE_NOT_FOUND => {
                    result.removed_defaults += 1;
                    result.steps.push(ReconcileStep {
                        op: "delete_default".into(),
                        target: format!("if#{}", r.if_index),
                        detail: dualnic_core::msgref!("DNLOG", 61; &r.gateway.to_string()),
                    });
                }
                ACCESS_DENIED => return permission_result(),
                rc => tracing::warn!(if_index = r.if_index, rc, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 45;))),
            }
        }
        // 再确保 WAN 有默认：WAN 解析成功，但当前 WAN 卡上没有默认 → 补一条。
        // 下一跳优先取 WAN 卡的 DHCP 服务器（注册表 DhcpServer）——比 gateway_ipv4 更可靠：
        // WAN 卡若连默认路由都丢了，其 gateway_ipv4 可能也变 None，但 DHCP 服务器地址（租约）仍保留。
        if let Some(wan) = &report.wan {
            let wan_has_default = routes.iter().any(|r| r.is_default() && r.if_index == wan.if_index);
            // 闸：WAN 卡必须 oper_up（介质连接）才补默认 —— 断线卡写默认落不住，徒增每轮空写。
            let wan_usable = adapters.iter()
                .find(|a| a.if_index == wan.if_index)
                .is_some_and(|v| v.oper_up);
            if !wan_has_default && wan_usable {
                let dhcp = adapters.iter().find(|a| a.if_index == wan.if_index).and_then(|a| a.dhcp_server);
                let gw = dhcp.or(wan.gateway); // 优先 DHCP 服务器，回退 gateway_ipv4
                if let Some(gw) = gw {
                    let row = make_prefix_row(Ipv4Addr::UNSPECIFIED, 0, gw, wan.if_index, cfg.interface_metric.wan, true);
                    match unsafe { CreateIpForwardEntry(&row) } {
                        0 => {
                            result.added_prefixes += 1;
                            result.steps.push(ReconcileStep {
                                op: "add_default".into(),
                                target: format!("if#{}", wan.if_index),
                                detail: dualnic_core::msgref!("DNLOG", 62; &gw.to_string()),
                            });
                        }
                        // 已存在：不计新增（避免每轮虚报「加 1 条」）。
                        ERROR_OBJECT_ALREADY_EXISTS => {
                            tracing::debug!(if_index = wan.if_index, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 46;)));
                        }
                        ACCESS_DENIED => return permission_result(),
                        rc => tracing::warn!(if_index = wan.if_index, rc, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 47;))),
                    }
                } else {
                    tracing::warn!(if_index = wan.if_index, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 48;)));
                }
            }
        }
    }

    // ③ 清理「带网关 + 非默认 + 不在内网清单 + 不在受保护接口」的静态前缀路由（manual 遗留路由）。
    //    **务必用 is_direct() 排除直连路由**：直连路由的 next_hop 是接口自身 IP（非 0.0.0.0），
    //    只看 next_hop 会把 198.51.100.0/23、10.255.0.0/24 等直连误删，导致断网 —— 这是上一轮事故的根因。
    //    只在对账开启时执行；否则会误删用户手动/自动想保留的前缀。
    if cfg.reconciliation.converge_default_route || cfg.reconciliation.remove_stale_defaults {
        for r in routes.iter().filter(|r| r.mask_len > 0 && !r.is_direct()) {
            let prefix = match Prefix::new(r.dest, r.mask_len) {
                Ok(p) => p,
                Err(_) => continue,
            };
            // 内网清单内的网段 → 不走删除（交给 diff 的 add_prefix），尝试跳过
            if cfg.lan_networks.iter().any(|n| same_net(&n.cidr, &prefix)) {
                continue;
            }
            // 未识别接口（不在枚举里）→ 不认识，保留不删；受保护接口 → 绝不删。
            let Some(view) = adapters.iter().find(|a| a.if_index == r.if_index) else {
                tracing::warn!(if_index = r.if_index, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 49;)));
                continue;
            };
            if is_protected(&protected, view.guid.as_deref()) {
                continue;
            }
            let row = make_delete_row(r.dest, r.mask_len, r.gateway, r.if_index);
            match unsafe { DeleteIpForwardEntry(&row) } {
                0 | ERROR_NOT_FOUND | ERROR_FILE_NOT_FOUND => {
                    result.removed_static_prefixes += 1;
                    result.steps.push(ReconcileStep {
                        op: "delete_static_prefix".into(),
                        target: prefix.to_string(),
                        detail: dualnic_core::msgref!("DNLOG", 63; &r.gateway.to_string(), &r.if_index.to_string()),
                    });
                }
                ACCESS_DENIED => return permission_result(),
                rc => tracing::warn!(dest = %prefix, rc, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 50;))),
            }
        }
    }

    for a in &actions {
        match a.kind {
            DiffKind::StaleDefaultOnOther => {
                // converge 开启时 step ② 已删完全部非 WAN 默认 → 跳过，避免重复删除双计。
                if cfg.reconciliation.converge_default_route {
                    continue;
                }
                // 仅 remove_stale 单开（step ② 未跑）时由此删除（非 WAN 且非 protected 的 0.0.0.0/0）
                let (dest, mask, gw) = (Ipv4Addr::UNSPECIFIED, 0u8, a.gateway.unwrap_or(Ipv4Addr::UNSPECIFIED));
                let row = make_delete_row(dest, mask, gw, a.if_index);
                match unsafe { DeleteIpForwardEntry(&row) } {
                    0 | ERROR_NOT_FOUND | ERROR_FILE_NOT_FOUND => {
                        result.removed_defaults += 1;
                        result.steps.push(ReconcileStep {
                            op: "delete_default".into(),
                            target: format!("if#{}", a.if_index),
                            detail: dualnic_core::msgref!("DNLOG", 64; &gw.to_string()),
                        });
                    }
                    ACCESS_DENIED => return permission_result(),
                    rc => {
                        result.steps.push(ReconcileStep {
                            op: "delete_default".into(),
                            target: format!("if#{}", a.if_index),
                            detail: dualnic_core::msgref!("DNLOG", 65; &rc.to_string()),
                        });
                    }
                }
            }
            DiffKind::MissingPrefixOnLan => {
                // 加内网前缀路由（有网关→INDIRECT，无网关→DIRECT on-link）
                // metric 用 LAN 接口的固定值：此时接口 metric 已落地为该值，dwForwardMetric1 = 接口 metric
                // （路由自身 metric=0）满足「≥ 接口 metric」约束。
                let Some(dest) = a.dest else { continue };
                let gw = a.gateway.unwrap_or(Ipv4Addr::UNSPECIFIED);
                let gw_present = a.gateway.is_some();
                let row = make_prefix_row(dest.addr, dest.len, gw, a.if_index, cfg.interface_metric.lan, gw_present);
                match unsafe { CreateIpForwardEntry(&row) } {
                    0 => {
                        result.added_prefixes += 1;
                        result.steps.push(ReconcileStep {
                            op: "add_prefix".into(),
                            target: dest.to_string(),
                            detail: dualnic_core::msgref!("DNLOG", 68; &a.if_index.to_string(), &gw.to_string()),
                        });
                        let rk = RouteKey {
                            dest: dest.addr,
                            mask_len: dest.len,
                            gateway: gw,
                            if_index: a.if_index,
                        };
                        if !manifest.added_routes.contains(&rk) {
                            manifest.added_routes.push(rk);
                        }
                    }
                    // 已存在：不计新增、也不进 manifest —— 这行路由可能并非本工具写入，
                    // 记进 manifest 会导致切「不管理」方案时误删他人路由。
                    ERROR_OBJECT_ALREADY_EXISTS => {
                        tracing::debug!(target = %dest, if_index = a.if_index, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 51;)));
                    }
                    ACCESS_DENIED => return permission_result(),
                    rc => {
                        result.steps.push(ReconcileStep {
                            op: "add_prefix".into(),
                            target: dest.to_string(),
                            detail: dualnic_core::msgref!("DNLOG", 66; &rc.to_string()),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    save_manifest(&manifest);
    result
}

// ──────────────────────────── 行构造 ────────────────────────────

fn ip_field(ip: Ipv4Addr) -> u32 {
    u32::from(ip).to_be()
}
fn mask_field(len: u8) -> u32 {
    let m = if len == 0 { 0 } else { u32::MAX << (32 - len as u32) };
    m.to_be()
}

fn make_prefix_row(dest: Ipv4Addr, mask_len: u8, next_hop: Ipv4Addr, if_index: u32, metric: u32, gw_present: bool) -> MIB_IPFORWARDROW {
    let mut row = MIB_IPFORWARDROW::default();
    row.dwForwardDest = ip_field(dest);
    row.dwForwardMask = mask_field(mask_len);
    row.dwForwardPolicy = 0;
    row.dwForwardNextHop = ip_field(next_hop);
    row.dwForwardIfIndex = if_index;
    row.Anonymous1.dwForwardType = if gw_present {
        MIB_IPROUTE_TYPE_INDIRECT.0 as u32
    } else {
        MIB_IPROUTE_TYPE_DIRECT.0 as u32
    };
    row.Anonymous2.dwForwardProto = MIB_IPPROTO_NETMGMT.0 as u32;
    row.dwForwardAge = 0;
    row.dwForwardNextHopAS = 0;
    row.dwForwardMetric1 = metric;
    row.dwForwardMetric2 = MIB_IPROUTE_METRIC_UNUSED;
    row.dwForwardMetric3 = MIB_IPROUTE_METRIC_UNUSED;
    row.dwForwardMetric4 = MIB_IPROUTE_METRIC_UNUSED;
    row.dwForwardMetric5 = MIB_IPROUTE_METRIC_UNUSED;
    row
}

fn make_delete_row(dest: Ipv4Addr, mask_len: u8, next_hop: Ipv4Addr, if_index: u32) -> MIB_IPFORWARDROW {
    let mut row = MIB_IPFORWARDROW::default();
    row.dwForwardDest = ip_field(dest);
    row.dwForwardMask = mask_field(mask_len);
    row.dwForwardPolicy = 0;
    row.dwForwardNextHop = ip_field(next_hop);
    row.dwForwardIfIndex = if_index;
    row
}

// ──────────────────────────── metric ────────────────────────────

/// `set_interface_metric` 的结果，区分「幂等未写」与「实际写入」，避免对账误报「固定 metric」。
enum MetricOutcome {
    /// 已是目标值，未写任何东西。
    Unchanged,
    /// 实际写入成功（SetIpInterfaceEntry 返回 0）。
    Written,
    /// 写入返回非 0 码（含 ACCESS_DENIED=5）。
    Failed(u32),
}

fn set_interface_metric(if_index: u32, metric: u32, use_auto: bool) -> Result<MetricOutcome, String> {
    let mut luid = NET_LUID_LH::default();
    let rc = unsafe { ConvertInterfaceIndexToLuid(if_index, &mut luid) };
    if rc.0 != 0 {
        return Err(dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 53; &format!("{rc:?}"))));
    }
    // 读当前值做幂等判断（只读探测）
    let mut probe = MIB_IPINTERFACE_ROW::default();
    unsafe { InitializeIpInterfaceEntry(&mut probe) };
    probe.Family = AF_INET;
    probe.InterfaceLuid = luid;
    let rc = unsafe { GetIpInterfaceEntry(&mut probe) };
    if rc.0 != 0 {
        return Err(dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 54; &format!("{rc:?}"))));
    }
    tracing::info!(
        if_index,
        luid = unsafe { luid.Value },
        idx = probe.InterfaceIndex,
        metric_now = probe.Metric,
        auto_now = probe.UseAutomaticMetric.0,
        "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 55;))
    );
    if !use_auto && probe.Metric == metric && probe.UseAutomaticMetric.0 == 0 {
        return Ok(MetricOutcome::Unchanged); // 幂等
    }
    if use_auto && probe.UseAutomaticMetric.0 != 0 && probe.Metric == metric {
        return Ok(MetricOutcome::Unchanged);
    }

    // 用「干净 row」写：InitializeIpInterfaceEntry 默认值 + 仅设 Family/Luid/Metric/UseAutomaticMetric。
    // 若直接把 GetIpInterfaceEntry 填满的 row 交给 SetIpInterfaceEntry，其只读/保留字段会触发
    // ERROR_INVALID_PARAMETER(87)。
    let mut row = MIB_IPINTERFACE_ROW::default();
    unsafe { InitializeIpInterfaceEntry(&mut row) };
    row.Family = AF_INET;
    row.InterfaceLuid = luid;
    row.Metric = metric;
    row.UseAutomaticMetric = BOOLEAN(if use_auto { 1 } else { 0 });
    let rc = unsafe { SetIpInterfaceEntry(&mut row) };
    tracing::info!(if_index, rc = rc.0, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 52;)));
    if rc.0 == 0 {
        Ok(MetricOutcome::Written)
    } else {
        Ok(MetricOutcome::Failed(rc.0))
    }
}

// ──────────────────────────── manifest ────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RouteKey {
    dest: Ipv4Addr,
    mask_len: u8,
    gateway: Ipv4Addr,
    if_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RouteManifest {
    added_routes: Vec<RouteKey>,
}

// ──────────────────────────── manifest 读写 ────────────────────────────

fn load_manifest() -> RouteManifest {
    std::fs::read_to_string(crate::paths::route_manifest_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_manifest(m: &RouteManifest) {
    if let Ok(s) = serde_json::to_string_pretty(m) {
        let _ = std::fs::write(crate::paths::route_manifest_path(), s);
    }
}

// ──────────────────────────── 辅助 ────────────────────────────

fn is_protected(protected: &[String], guid: Option<&str>) -> bool {
    let Some(guid) = guid else { return false };
    protected.iter().any(|p| normalize_guid(p) == normalize_guid(guid))
}

/// 两个网段是否同一网络（网络地址 + 前缀长度一致）。
fn same_net(a: &Prefix, b: &Prefix) -> bool {
    a.addr == b.addr && a.len == b.len
}

fn err_result(msg: dualnic_core::msg::MessageRef) -> ReconcileResult {
    ReconcileResult { ok: false, removed_defaults: 0, removed_static_prefixes: 0, added_prefixes: 0, metrics_fixed: 0, steps: vec![], error: Some(msg) }
}

fn permission_result() -> ReconcileResult {
    ReconcileResult {
        ok: false,
        removed_defaults: 0,
        removed_static_prefixes: 0,
        added_prefixes: 0,
        metrics_fixed: 0,
        steps: vec![],
        error: Some(dualnic_core::msgref!("DNREC", 18;)),
    }
}
