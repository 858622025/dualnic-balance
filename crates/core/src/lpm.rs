//! 最长前缀匹配（LPM）查询：输入任意 IPv4，决定它命中哪条策略 → 走哪块卡。
//!
//! 这是 GUI「规则预览页」与 core 自测的核心：与 Windows 路由表真实决策一致
//! （Windows 也用 LPM 选路由），因此这里的结果可以直接映射到「将走哪块卡」。
//!
//! 用简单的线性扫描 + 显式 `0.0.0.0/0` 兜底即可：策略网段通常只有几十条，
//! 不需要 trie；保持简单可测优先。

use std::net::Ipv4Addr;

use crate::config::LanNetwork;
use crate::ip::Prefix;
use crate::role::AdapterRole;

/// LPM 查询结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LpmDecision {
    /// 命中某条内网前缀 → 走内网（LAN）卡。
    /// 携带命中的前缀，GUI 可展示「最长前缀是哪一条」。
    Lan(Prefix),
    /// 未命中任何内网前缀 → 走唯一默认路由 → 外网（WAN）卡。
    Wan,
}

/// 根据「内网网段表」判定某个 IPv4 走哪块卡。
///
/// 规则与真实路由一致：取所有命中的内网前缀中最长的一个；
/// 若没有任何命中，则落默认路由（外网）。`lan_networks` 中不应含 /0
/// （config 层已禁止），但为健壮起见这里仍按「/0 视为默认」处理。
pub fn decide_lpm(lan_networks: &[LanNetwork], ip: Ipv4Addr) -> LpmDecision {
    let mut best: Option<Prefix> = None;
    for net in lan_networks {
        if net.cidr.contains(ip) {
            // 取最长前缀（len 最大）
            match best {
                Some(cur) if cur.len >= net.cidr.len => {}
                _ => best = Some(net.cidr),
            }
        }
    }
    match best {
        Some(p) if p.len > 0 => LpmDecision::Lan(p),
        // 无命中或命中 /0（理论不应发生）→ 都走外网
        _ => LpmDecision::Wan,
    }
}

/// 便捷封装：决定结果是否应走某角色。
impl LpmDecision {
    pub fn role(&self) -> AdapterRole {
        match self {
            LpmDecision::Lan(_) => AdapterRole::Lan,
            LpmDecision::Wan => AdapterRole::Wan,
        }
    }
}
