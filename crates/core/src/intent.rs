//! 路由「期望态」模型：把 PolicyConfig + 当前网卡快照，翻译成希望系统路由表
//! 呈现的确定性目标（对应源设计文档 §3 方案 1 的收敛原则）。
//!
//! 原则：全系统**只有一条**默认路由（挂 WAN 卡）+ 每条内网网段一条前缀静态路由
//! （挂 LAN 卡，LPM 优先，不参与 metric 摇摆）。这里产出的是「意图描述」，
//! 由 service 层执行成真实的 SetIpForwardEntry2 / route 操作，并对账。

use std::net::Ipv4Addr;

use crate::config::PolicyConfig;
use crate::ip::Prefix;

/// 一条期望路由的语义描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteIntent {
    /// 全系统唯一默认路由：0.0.0.0/0 → WAN 网卡网关。
    DefaultViaWan { gw: Ipv4Addr },
    /// 内网前缀静态路由：prefix → LAN 网卡网关。
    PrefixToLan { prefix: Prefix, gw: Ipv4Addr },
}

/// 由策略推导出的完整期望路由集合。
#[derive(Debug, Clone, Default)]
pub struct RouteIntentSet {
    /// 默认路由（最多一条）。
    pub default: Option<RouteIntent>,
    /// 内网前缀路由（按前缀长度降序，便于审计）。
    pub prefixes: Vec<RouteIntent>,
}

impl RouteIntentSet {
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.prefixes.is_empty()
    }
}

/// 从配置 + 已识别出的两块物理网卡的网关，构建期望态。
///
/// 参数由 service 层提供（它负责真正枚举网卡、读当前网关、匹配角色）。
pub fn build_intent(
    cfg: &PolicyConfig,
    resolved: &ResolvedAdapters,
) -> Result<RouteIntentSet, crate::CoreError> {
    // 校验规则：WAN 必须拿到网关（它背默认路由）；LAN 至少得有网关或接口。
    let wan_gw = resolved
        .wan_gateway
        .ok_or_else(|| crate::CoreError::Msg(crate::msgref!("DNERR", 50;)))?;
    let lan_gw = resolved
        .lan_gateway
        .ok_or_else(|| crate::CoreError::Msg(crate::msgref!("DNERR", 51;)))?;

    let mut set = RouteIntentSet::default();
    set.default = Some(RouteIntent::DefaultViaWan { gw: wan_gw });

    // 内网前缀按 len 降序排序，审计/落地顺序更接近 Windows 展示习惯。
    let mut nets: Vec<_> = cfg.lan_networks.iter().collect();
    nets.sort_by_key(|n| std::cmp::Reverse(n.cidr.len));
    for n in nets {
        set.prefixes.push(RouteIntent::PrefixToLan { prefix: n.cidr, gw: lan_gw });
    }
    Ok(set)
}

/// service 层解析结果：把 PolicyConfig 里的角色规则落到真实网卡上之后，
/// 拿到两块物理卡的网关。网关在 Windows 上通常 = DHCP 给的默认网关。
#[derive(Debug, Clone)]
pub struct ResolvedAdapters {
    /// WAN 网卡的当前网关（默认路由下一跳）。
    pub wan_gateway: Option<Ipv4Addr>,
    /// LAN 网卡的当前网关（内网前缀下一跳）。
    pub lan_gateway: Option<Ipv4Addr>,
}

impl ResolvedAdapters {
    pub fn new(wan: Option<Ipv4Addr>, lan: Option<Ipv4Addr>) -> Self {
        ResolvedAdapters { wan_gateway: wan, lan_gateway: lan }
    }
}

/// 供 GUI / 诊断用：人类可读的意图描述。
impl std::fmt::Display for RouteIntentSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.default {
            Some(RouteIntent::DefaultViaWan { gw }) => {
                writeln!(f, "{}", crate::msg::t(&crate::msgref!("DNLOG", 85; &gw.to_string())))
            }
            _ => writeln!(f, "{}", crate::msg::t(&crate::msgref!("DNLOG", 86;))),
        }?;
        for r in &self.prefixes {
            if let RouteIntent::PrefixToLan { prefix, gw } = r {
                writeln!(f, "{}", crate::msg::t(&crate::msgref!("DNLOG", 87; &prefix.to_string(), &gw.to_string())))?;
            }
        }
        Ok(())
    }
}
