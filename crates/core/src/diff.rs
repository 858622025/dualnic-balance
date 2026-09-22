//! 只读「网络快照 → 意图 vs 实际 diff」纯逻辑。
//!
//! 输入：系统枚举出的网卡视图 + 全部 IPv4 路由（service 层从 Windows 读取后以纯类型注入）；
//! 输出：角色解析结果 + 只读建议/观察动作列表。**本模块绝不包含任何写路由语义**——
//! `DiffAction.recommended` 只表达“执行器开启相应开关后会处理”，本身不含执行指令。

use std::collections::HashSet;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::config::{LanNetwork, PolicyConfig};
use crate::ip::Prefix;
use crate::role::{normalize_guid, resolve_role, AdapterProbe, AdapterRole};

/// 一块网卡的系统视图（纯数据，由 service 从 Windows 组装）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterView {
    pub if_index: u32,
    /// 网卡永久 GUID（可能有/无花括号，展示时保留原样，比较走 normalize）。
    pub guid: Option<String>,
    /// 驱动描述（如 "Intel(R) Wi-Fi 6E AX210 160MHz"）。
    pub description: Option<String>,
    /// 连接名（如 "WLAN" / "以太网"）。
    pub alias: Option<String>,
    pub primary_ipv4: Option<Ipv4Addr>,
    pub gateway_ipv4: Option<Ipv4Addr>,
    /// 该网卡的 DHCP 服务器（registry DhcpServer，服务端填充）；用于手工网段的下一跳。
    #[serde(default)]
    pub dhcp_server: Option<Ipv4Addr>,
    /// 首个 IPv4 单播的前缀长度（如 /24）；未知为 None。
    #[serde(default)]
    pub prefix_len: Option<u8>,
    /// 接口类型（GetAdaptersAddresses.IfType：6=以太网、71=WLAN、131=隧道、24=回环…）。
    /// GUI 据此把物理卡（6/71）与虚拟/其它分组。未知为 None。
    #[serde(default)]
    pub if_type: Option<u32>,
    pub oper_up: bool,
}

/// 一条「当前真实可达网段」的推导来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReachSource {
    /// 由该卡首个 IPv4 单播地址 + 前缀长度推导（权威）。
    AdapterPrimary,
    /// 由路由表 on-link（网关 0.0.0.0）段兜底。
    OnLinkRoute,
}

/// 探测到的真实可达网段（供 GUI 展示 / 采纳进内网段）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachableNet {
    pub if_index: u32,
    pub prefix: Prefix,
    pub source: ReachSource,
}

/// 一条 IPv4 路由（全表，不只默认）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteView {
    pub if_index: u32,
    pub dest: Ipv4Addr,
    /// 前缀长度（0 = 默认路由）。
    pub mask_len: u8,
    /// 下一跳（0.0.0.0 = on-link）。
    pub gateway: Ipv4Addr,
    pub metric: u32,
    /// dwForwardProto。
    pub source_proto: u32,
    /// dwForwardType：3=MIB_IPROUTE_TYPE_DIRECT(直连 on-link)、4=MIB_IPROUTE_TYPE_INDIRECT(经网关)。
    /// 旧序列化无此字段 → 缺省 0。判定「是否直连」必须用它（或 next_hop=0），
    /// 因为对 on-link 直连路由，GetIpForwardTable 的 dwForwardNextHop 会是接口自身 IP 而非 0.0.0.0，
    /// 只看 next_hop 会把直连误判成「带网关」从而误删。
    #[serde(default)]
    pub forward_type: u32,
}

impl RouteView {
    pub fn is_default(&self) -> bool {
        self.mask_len == 0
    }
    /// 是否直连（on-link）：dwForwardType=DIRECT(3) 或（缺省时）下一跳为 0.0.0.0。
    /// 直连路由**绝不**删除 —— 删掉会断网卡本地链路。
    pub fn is_direct(&self) -> bool {
        self.forward_type == 3 || (self.forward_type == 0 && self.gateway == Ipv4Addr::UNSPECIFIED)
    }
}

/// 已解析的角色卡（供 GUI 画两块物理卡）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedCard {
    pub if_index: u32,
    pub guid: Option<String>,
    pub alias: Option<String>,
    pub description: Option<String>,
    pub ip: Option<Ipv4Addr>,
    pub gateway: Option<Ipv4Addr>,
}

/// diff 动作种类（只读语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    /// ✓ 期望默认已在 WAN 卡就位。
    KeepDefaultOnWan,
    /// ✗ WAN 已解析但没有默认路由。
    MissingDefaultOnWan,
    /// 非 WAN、未受保护接口上的多余默认：建议删 / 仅观察。
    StaleDefaultOnOther,
    /// 受保护或未知接口出现默认：保留 + 告警，绝不删。
    ProtectedDefaultSeen,
    /// ✓ 期望内网前缀已走 LAN。
    KeepPrefixOnLan,
    /// ✗ 期望前缀缺失，或仅部分覆盖。
    MissingPrefixOnLan,
    /// 前缀存在但下一跳在非 LAN 卡。
    PrefixViaWrongIface,
}

/// 一条 diff 动作。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffAction {
    pub kind: DiffKind,
    pub if_index: u32,
    pub guid: Option<String>,
    /// 相关网络（0.0.0.0/0 或配置内网段）。
    pub dest: Option<Prefix>,
    pub gateway: Option<Ipv4Addr>,
    pub metric: u32,
    pub source_proto: u32,
    /// 语义是否已达成（GUI 绿）。
    pub ok: bool,
    /// 对账执行器开启相应开关后是否会处理；false = 仅观察 / 不可执行。
    pub recommended: bool,
    pub note: Option<crate::msg::MessageRef>,
}

/// 完整 diff 报告。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiffReport {
    /// 全部网卡视图（含虚拟卡，供 GUI 审计 / 抄 GUID 进 protected）。
    pub adapters: Vec<AdapterView>,
    pub wan: Option<ResolvedCard>,
    pub lan: Option<ResolvedCard>,
    /// 角色解析失败原因（缺失 / 歧义）；此时不 panic，动作降级为观察。
    pub resolve_error: Option<String>,
    pub actions: Vec<DiffAction>,
    /// 探测到的真实可达网段（按 if_index、前缀长度排序）。
    #[serde(default)]
    pub reachable: Vec<ReachableNet>,
    /// 配置的内网网段表（供 GUI「输入任意 IP 查走哪块卡」的 LPM 可视化）。
    #[serde(default)]
    pub lan_networks: Vec<LanNetwork>,
}

// ──────────────────────────── 入口 ────────────────────────────

pub fn diff_snapshot(cfg: &PolicyConfig, adapters: &[AdapterView], routes: &[RouteView]) -> DiffReport {
    let mut report = DiffReport {
        adapters: adapters.to_vec(),
        lan_networks: cfg.lan_networks.clone(),
        ..Default::default()
    };

    // 1) 角色解析：规则为空 = 该侧不管理（WAN/LAN 各自独立；哪侧为空就不解析、也不报错）。
    let probes: Vec<AdapterProbe> = adapters.iter().map(probe_of).collect();
    let mut resolve_error: Option<String> = None;
    let mut wan = None;
    let mut lan = None;
    if !cfg.wan_adapter.matchers.is_empty() {
        match resolve_role(AdapterRole::Wan, &cfg.wan_adapter, &probes) {
            Ok(r) => wan = view_for(&probes, r, adapters).map(to_card),
            Err(e) => resolve_error = Some(e.to_string()),
        }
    }
    if !cfg.lan_adapter.matchers.is_empty() {
        match resolve_role(AdapterRole::Lan, &cfg.lan_adapter, &probes) {
            Ok(r) => lan = view_for(&probes, r, adapters).map(to_card),
            Err(e) => {
                if resolve_error.is_none() {
                    resolve_error = Some(e.to_string());
                }
            }
        }
    }
    report.wan = wan.clone();
    report.lan = lan.clone();
    report.resolve_error = resolve_error;

    // 2) 默认路由
    collect_default_actions(cfg, adapters, routes, wan.as_ref(), lan.as_ref(), &mut report.actions);
    // 3) 内网前缀
    collect_prefix_actions(cfg, adapters, routes, wan.as_ref(), lan.as_ref(), &mut report.actions);
    // 4) 真实可达网段（供 GUI 展示 / 采纳）
    report.reachable = collect_reachable(adapters, routes);

    report
}

/// 探测真实可达网段：主源=各卡单播地址+前缀；兜底=路由表 on-link 段。
/// 过滤 /32 主机、loopback 接口、组播等噪音；按 (if_index, prefix) 去重。
fn collect_reachable(adapters: &[AdapterView], routes: &[RouteView]) -> Vec<ReachableNet> {
    let mut seen: HashSet<(u32, Prefix)> = HashSet::new();
    let mut out: Vec<ReachableNet> = Vec::new();

    // 1) 卡单播主源
    for a in adapters {
        if a.if_index == 1 {
            continue; // loopback
        }
        if let (Some(ip), Some(len)) = (a.primary_ipv4, a.prefix_len) {
            if len == 0 || len > 30 || is_link_local(ip) {
                continue;
            }
            if let Ok(p) = Prefix::new(ip, len) {
                if seen.insert((a.if_index, p)) {
                    out.push(ReachableNet { if_index: a.if_index, prefix: p, source: ReachSource::AdapterPrimary });
                }
            }
        }
    }

    // 2) on-link 路由兜底
    for r in routes {
        if r.is_default() || r.gateway != Ipv4Addr::UNSPECIFIED {
            continue;
        }
        if !(8..=30).contains(&r.mask_len) {
            continue;
        }
        if is_loopback_if(adapters, r.if_index) || is_multicast(r.dest) || is_link_local(r.dest) {
            continue;
        }
        let p = match Prefix::new(r.dest, r.mask_len) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if seen.insert((r.if_index, p)) {
            out.push(ReachableNet { if_index: r.if_index, prefix: p, source: ReachSource::OnLinkRoute });
        }
    }

    out.sort_by_key(|n| (n.if_index, std::cmp::Reverse(n.prefix.len)));
    out
}

fn is_loopback_if(adapters: &[AdapterView], if_index: u32) -> bool {
    if if_index == 1 {
        return true;
    }
    adapters.iter().any(|a| {
        a.if_index == if_index
            && [a.alias.as_deref(), a.description.as_deref()]
                .iter()
                .flatten()
                .any(|s| s.to_lowercase().contains("loopback"))
    })
}

fn is_multicast(a: Ipv4Addr) -> bool {
    let o = a.octets();
    (224..240).contains(&o[0])
}

/// 169.254.0.0/16：APIPA / link-local，不属“可路由真实网段”，作噪音滤掉。
fn is_link_local(a: Ipv4Addr) -> bool {
    let o = a.octets();
    o[0] == 169 && o[1] == 254
}

// ──────────────────────────── 默认路由 ────────────────────────────

fn collect_default_actions(
    cfg: &PolicyConfig,
    adapters: &[AdapterView],
    routes: &[RouteView],
    wan: Option<&ResolvedCard>,
    lan: Option<&ResolvedCard>,
    out: &mut Vec<DiffAction>,
) {
    let defaults: Vec<&RouteView> = routes.iter().filter(|r| r.is_default()).collect();
    let wan_ok = wan.is_some_and(|c| defaults.iter().any(|r| r.if_index == c.if_index));

    if defaults.is_empty() {
        if let Some(c) = wan {
            // WAN 应持默认但一个都没有（观察：自动修复需稳定网关，属执行器决策）。
            out.push(DiffAction {
                kind: DiffKind::MissingDefaultOnWan,
                if_index: c.if_index,
                guid: c.guid.clone(),
                dest: Some("0.0.0.0/0".parse().expect("const literal")),
                gateway: None,
                metric: 0,
                source_proto: 0,
                ok: false,
                recommended: false,
                note: Some(crate::msgref!("DNDIF", 1;)),
            });
        }
        return;
    }

    for r in defaults {
        let view = adapter_by_index(adapters, r.if_index);
        let (kind, ok, recommended, note) = if let Some(c) = wan {
            if r.if_index == c.if_index {
                (DiffKind::KeepDefaultOnWan, true, false, None)
            } else if is_protected(cfg, view).unwrap_or(false) || view.is_none() {
                // 受保护或未知接口（如多 compartment）：保留 + 告警，绝不删。
                (DiffKind::ProtectedDefaultSeen, false, false,
                 Some(if view.is_none() {
                     crate::msgref!("DNDIF", 3;)
                 } else {
                     crate::msgref!("DNDIF", 4;)
                 }))
            } else {
                let recommended = cfg.reconciliation.converge_default_route
                    && cfg.reconciliation.remove_stale_defaults;
                let n = if lan.is_some_and(|l| r.if_index == l.if_index) {
                    crate::msgref!("DNDIF", 6; &r.metric)
                } else {
                    crate::msgref!("DNDIF", 5; &r.metric)
                };
                (DiffKind::StaleDefaultOnOther, false, recommended, Some(n))
            }
        } else {
            // 角色未解析：能明确分类的只有受保护/未知；其余仅观察，避免瞎猜 WAN。
            if is_protected(cfg, view).unwrap_or(false) || view.is_none() {
                (DiffKind::ProtectedDefaultSeen, false, false,
                 Some(crate::msgref!("DNDIF", 4;)))
            } else {
                (DiffKind::StaleDefaultOnOther, false, false,
                 Some(crate::msgref!("DNDIF", 7;)))
            }
        };

        out.push(DiffAction {
            kind,
            if_index: r.if_index,
            guid: view.and_then(|v| v.guid.clone()),
            dest: Some("0.0.0.0/0".parse().expect("const literal")),
            gateway: Some(r.gateway),
            metric: r.metric,
            source_proto: r.source_proto,
            ok,
            recommended,
            note,
        });
    }

    // WAN 存在但没吃到默认（defaults 非空，都在别处）
    if !wan_ok {
        if let Some(c) = wan {
            out.push(DiffAction {
                kind: DiffKind::MissingDefaultOnWan,
                if_index: c.if_index,
                guid: c.guid.clone(),
                dest: Some("0.0.0.0/0".parse().expect("const literal")),
                gateway: None,
                metric: 0,
                source_proto: 0,
                ok: false,
                recommended: false,
                note: Some(crate::msgref!("DNDIF", 2;)),
            });
        }
    }
}

// ──────────────────────────── 内网前缀 ────────────────────────────

fn collect_prefix_actions(
    cfg: &PolicyConfig,
    adapters: &[AdapterView],
    routes: &[RouteView],
    wan: Option<&ResolvedCard>,
    lan: Option<&ResolvedCard>,
    out: &mut Vec<DiffAction>,
) {
    let _ = wan; // 前缀只与 LAN 相关
    let mut nets: Vec<&crate::config::LanNetwork> = cfg.lan_networks.iter().collect();
    nets.sort_by_key(|n| std::cmp::Reverse(n.cidr.len));

    for net in nets {
        let p = net.cidr;
        // 等长精确命中？
        let exact: Vec<&RouteView> = routes
            .iter()
            .filter(|r| r.dest == p.addr && r.mask_len == p.len)
            .collect();

        let on_lan = lan.is_some_and(|c| exact.iter().any(|r| r.if_index == c.if_index));
        let action = if let Some(r) = exact.first() {
            if on_lan {
                DiffAction {
                    kind: DiffKind::KeepPrefixOnLan, if_index: r.if_index,
                    guid: adapter_by_index(adapters, r.if_index).and_then(|v| v.guid.clone()),
                    dest: Some(p), gateway: Some(r.gateway), metric: r.metric,
                    source_proto: r.source_proto, ok: true, recommended: false, note: None,
                }
            } else {
                // 存在等长路由但不在 LAN：提示走错卡（不删他人）。
                let view = adapter_by_index(adapters, r.if_index);
                DiffAction {
                    kind: DiffKind::PrefixViaWrongIface, if_index: r.if_index,
                    guid: view.and_then(|v| v.guid.clone()),
                    dest: Some(p), gateway: Some(r.gateway), metric: r.metric,
                    source_proto: r.source_proto, ok: false, recommended: false,
                    note: Some(crate::msgref!("DNDIF", 8; &p, &r.if_index)),
                }
            }
        } else {
            // 缺失：按是否绑定网卡分支决定「下一跳 / if_index / recommended」。
            //   - 手工网段（via_iface_guid 有值）：下一跳 = 该网卡的 DHCP 服务器；recommended 仅当
            //     「网卡能枚举到 且 dhcp_server 存在」——否则对账会跳过，绝不写 0.0.0.0 垃圾直连。
            //   - 自动网段（无 via_iface_guid）：随 LAN 卡 on-link 直连（维持现状）。
            // 是否存在更窄的 on-link 覆盖段（部分覆盖判定）。
            let covered: Vec<&RouteView> = routes
                .iter()
                .filter(|r| {
                    r.mask_len > p.len
                        && p.contains(r.dest)
                        && r.gateway == Ipv4Addr::UNSPECIFIED
                })
                .collect();
            let (if_index, guid, gw, recommended, note) =
                match &net.via_iface_guid {
                    Some(via_guid) => {
                        match adapter_by_guid(adapters, via_guid) {
                            Some(a) => {
                                // 闸：目标卡必须 oper_up（介质连接）才可写 —— 断线卡上的路由写不进/落不住，
                                // 否则对账每轮空写一遍（futile loop）。未连接 → 仅观察，卡恢复后自动继续。
                                let usable = a.oper_up;
                                let coverage = covered.iter()
                                    .map(|r| format!("{}/{}", r.dest, r.mask_len))
                                    .collect::<Vec<_>>().join(", ");
                                let alias = a.alias.as_deref().unwrap_or("#?").to_string();
                                let note = if !usable {
                                    crate::msgref!("DNDIF", 9; &alias)
                                } else {
                                    match a.dhcp_server {
                                        Some(dhcp) => {
                                            if coverage.is_empty() {
                                                crate::msgref!("DNDIF", 10; &alias, &dhcp)
                                            } else {
                                                crate::msgref!("DNDIF", 11; &alias, &coverage, &p, &dhcp)
                                            }
                                        }
                                        None => crate::msgref!("DNDIF", 12; &alias),
                                    }
                                };
                                (a.if_index, a.guid.clone(), a.dhcp_server, usable && a.dhcp_server.is_some(), Some(note))
                            }
                            None => (0, None, None, false, Some(crate::msgref!("DNDIF", 13; &via_guid))),
                        }
                    }
                    None => {
                        let lan_gw = lan.and_then(|c| c.gateway);
                        // 闸：LAN 卡必须 oper_up（介质连接）—— 断线卡写 on-link 直连同样落不住（futile loop）。
                        let lan_usable = lan.is_some_and(|c| {
                            adapter_by_index(adapters, c.if_index).is_some_and(|v| v.oper_up)
                        });
                        let alias = lan.as_ref().map(|c| c.alias.as_deref().unwrap_or("#?").to_string());
                        let note = if lan.is_some() {
                            let alias = alias.as_deref().unwrap_or("#?");
                            if !lan_usable {
                                crate::msgref!("DNDIF", 14; &alias)
                            } else {
                                let coverage = covered.iter()
                                    .map(|r| format!("{}/{}", r.dest, r.mask_len))
                                    .collect::<Vec<_>>().join(", ");
                                match lan_gw {
                                    Some(_) => {
                                        if coverage.is_empty() {
                                            crate::msgref!("DNDIF", 15; &alias, &p)
                                        } else {
                                            crate::msgref!("DNDIF", 16; &coverage, &p)
                                        }
                                    }
                                    None => crate::msgref!("DNDIF", 17; &alias),
                                }
                            }
                        } else {
                            crate::msgref!("DNDIF", 18;)
                        };
                        // 无网关的 on-link 前缀也可执行（DIRECT 路由），但仅当 LAN 卡已连接。
                        (lan.as_ref().map(|c| c.if_index).unwrap_or(0), lan.as_ref().and_then(|c| c.guid.clone()), lan_gw, lan_usable, Some(note))
                    }
                };
            DiffAction {
                kind: DiffKind::MissingPrefixOnLan,
                if_index,
                guid,
                dest: Some(p), gateway: gw, metric: 0, source_proto: 0,
                ok: false, recommended, note,
            }
        };
        out.push(action);
    }
}

// ──────────────────────────── 小工具 ────────────────────────────

fn probe_of(v: &AdapterView) -> AdapterProbe {
    AdapterProbe {
        guid: v.guid.clone(),
        description: v.description.clone(),
        name: v.alias.clone(),
    }
}

/// 在 probe 列表里找与 resolved probe 相同的视图（resolve 返回的是候选副本）。
fn view_for<'a>(
    probes: &[AdapterProbe],
    want: &AdapterProbe,
    adapters: &'a [AdapterView],
) -> Option<&'a AdapterView> {
    probes.iter().position(|p| p == want).map(|i| &adapters[i])
}

fn to_card(v: &AdapterView) -> ResolvedCard {
    ResolvedCard {
        if_index: v.if_index,
        guid: v.guid.clone(),
        alias: v.alias.clone(),
        description: v.description.clone(),
        ip: v.primary_ipv4,
        gateway: v.gateway_ipv4,
    }
}

fn adapter_by_index(adapters: &[AdapterView], if_index: u32) -> Option<&AdapterView> {
    adapters.iter().find(|a| a.if_index == if_index)
}

/// 按归一化 GUID 匹配网卡（大小写/花括号不敏感）。
fn adapter_by_guid<'a>(adapters: &'a [AdapterView], guid: &str) -> Option<&'a AdapterView> {
    adapters
        .iter()
        .find(|a| a.guid.as_deref().is_some_and(|g| normalize_guid(g) == normalize_guid(guid)))
}

fn is_protected(cfg: &PolicyConfig, view: Option<&AdapterView>) -> Option<bool> {
    let guid = view.and_then(|v| v.guid.as_deref())?;
    Some(
        cfg.reconciliation
            .protected_interfaces
            .iter()
            .any(|g| normalize_guid(g) == normalize_guid(guid)),
    )
}
