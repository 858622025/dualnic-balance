//! 环境自动匹配：算「当前环境指纹」，与方案配置做全匹配（网卡身份 + DHCP 服务器）。
//!
//! 纯逻辑、无 Windows 依赖、可单测。数据来自 `diff::AdapterView`（service 从系统枚举注入）。
//! 绑定键 = GUID + DHCP 服务器：DHCP 服务器是「我在哪个网」的稳定身份；网段会随
//! DHCP 重分配 / APIPA 瞬态变化、且存在用户自定义可能 —— 一律不进校验。
//! 旧档（无 environment）永不匹配 → 永不自动切换/新建，退化为手动。

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::config::{AdapterEnv, EnvironmentSnapshot, PolicyConfig};
use crate::diff::AdapterView;
use crate::role::{normalize_guid, resolve_role, AdapterProbe, AdapterRole};

/// 单块网卡的环境指纹（按归一化 GUID 标识）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterFingerprint {
    /// 归一化 GUID（去花括号+小写）；无 GUID 时 None（此时只能靠描述，身份弱）。
    pub guid: Option<String>,
    /// 该卡的 DHCP 服务器（静态 IP 卡为 None）。
    pub dhcp_server: Option<Ipv4Addr>,
}

/// 当前环境的完整指纹（按 GUID 排序，保证可比较/幂等）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EnvFingerprint {
    pub adapters: Vec<AdapterFingerprint>,
}

/// 该网卡是否「用户标记的物理卡」（纯标记判定，不猜）。
/// 物理卡是硬件事实，由用户在 GUI 标记；空标记 = 无物理卡。彻底取代旧的 if_type+黑名单启发式。
pub fn is_physical_adapter(physical: &[String], a: &AdapterView) -> bool {
    a.guid.as_deref()
        .is_some_and(|g| physical.iter().any(|p| normalize_guid(p) == normalize_guid(g)))
}

/// 是否「已连接物理卡」= 被标记为物理 + oper_up + 有非 link-local IP + 非回环。
fn is_connected_physical(physical: &[String], a: &AdapterView) -> bool {
    is_physical_adapter(physical, a)
        && a.oper_up
        && a.if_index != 1
        && a.primary_ipv4.is_some_and(|ip| !is_link_local(ip))
}

fn is_link_local(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 169 && o[1] == 254
}

/// 已连接物理卡列表（供指纹计算 / 自动建方案预填复用）。
pub fn connected_physical<'a>(physical: &[String], adapters: &'a [AdapterView]) -> Vec<&'a AdapterView> {
    adapters.iter().filter(|a| is_connected_physical(physical, a)).collect()
}

/// 从真实网卡列表算当前环境指纹（只含已连接物理卡；键 = GUID + DHCP 服务器）。
pub fn compute_env_fingerprint(physical: &[String], adapters: &[AdapterView]) -> EnvFingerprint {
    let mut out: Vec<AdapterFingerprint> = adapters
        .iter()
        .filter(|a| is_connected_physical(physical, a))
        .map(|a| AdapterFingerprint {
            guid: a.guid.as_deref().map(normalize_guid),
            dhcp_server: a.dhcp_server,
        })
        .collect();
    out.sort_by(|x, y| x.guid.cmp(&y.guid));
    EnvFingerprint { adapters: out }
}

/// 匹配结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchResult {
    pub matched: bool,
    pub reason: Option<crate::msg::MessageRef>,
}

/// 新指纹的卡集合是否为方案 environment 卡集合的**真子集**（只少不多 = 疑似拔线/瞬态掉卡）。
/// 用于抑制 auto_match 的「0 匹配 → 自动新建」：掉卡瞬态不该产生垃圾方案。
/// 空指纹（全部标记卡都掉线）也算真子集 → 同样抑制。
pub fn is_card_loss_subset(fingerprint: &EnvFingerprint, env: &EnvironmentSnapshot) -> bool {
    let f: std::collections::HashSet<&str> =
        fingerprint.adapters.iter().filter_map(|a| a.guid.as_deref()).collect();
    let e: std::collections::HashSet<&str> =
        env.adapters.iter().map(|a| a.guid.as_str()).collect();
    f.is_subset(&e) && f.len() < e.len()
}

impl MatchResult {
    fn ok() -> Self {
        MatchResult { matched: true, reason: None }
    }
    fn fail(reason: crate::msg::MessageRef) -> Self {
        MatchResult { matched: false, reason: Some(reason) }
    }
}

/// 方案与当前环境是否全匹配（网卡身份 + 网段）。
pub fn match_profile(
    cfg: &PolicyConfig,
    fingerprint: &EnvFingerprint,
    adapters: &[AdapterView],
) -> MatchResult {
    // 旧档/未记录环境 → 永不匹配（也永不自动切/建）
    let Some(env) = cfg.environment.as_ref() else {
        return MatchResult::fail(crate::msgref!("DNLOG", 80;));
    };

    // 1) 卡组合一致：当前环境每块物理卡的「guid+DHCP服务器」== 方案记录的。
    //    两者都按 guid 排序（EnvFingerprint 已排序；env.adapters 保存时也排序），直接比相等。
    //    只用 guid+DHCP 服务器绑定：DHCP 服务器是「在哪个网」的稳定身份；
    //    网段会随 DHCP 重分配 / APIPA 瞬态变化，且用户可能自定义 —— 不进校验。
    let cur: Vec<AdapterEnv> = fingerprint
        .adapters
        .iter()
        .map(|a| AdapterEnv {
            guid: a.guid.clone().unwrap_or_default(),
            dhcp_server: a.dhcp_server,
        })
        .collect();
    if cur != env.adapters {
        return MatchResult::fail(crate::msgref!(
            "DNLOG", 81; &cur.len().to_string(), &env.adapters.len().to_string()
        ));
    }

    let probes: Vec<AdapterProbe> = adapters.iter().map(probe_of).collect();

    // 2) 角色解析（策略可行性）：LAN/WAN 卡在当前环境都存在即可。
    //    不再做「网段覆盖」校验 —— 网段是用户可自定义的派生物，不参与绑定。
    if !cfg.lan_adapter.matchers.is_empty() {
        let lan = match resolve_role(AdapterRole::Lan, &cfg.lan_adapter, &probes) {
            Ok(p) => view_for(&probes, p, adapters),
            Err(e) => return MatchResult::fail(crate::msgref!("DNLOG", 82; &e.to_string())),
        };
        if lan.is_none() {
            return MatchResult::fail(crate::msgref!("DNLOG", 83;));
        }
    }
    if !cfg.wan_adapter.matchers.is_empty() {
        if let Err(e) = resolve_role(AdapterRole::Wan, &cfg.wan_adapter, &probes) {
            return MatchResult::fail(crate::msgref!("DNLOG", 84; &e.to_string()));
        }
    }

    MatchResult::ok()
}

fn probe_of(v: &AdapterView) -> AdapterProbe {
    AdapterProbe {
        guid: v.guid.clone(),
        description: v.description.clone(),
        name: v.alias.clone(),
    }
}

fn view_for<'a>(probes: &[AdapterProbe], want: &AdapterProbe, adapters: &'a [AdapterView]) -> Option<&'a AdapterView> {
    probes.iter().position(|p| p == want).map(|i| &adapters[i])
}
