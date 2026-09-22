//! 网卡角色识别。
//!
//! 核心原则（源设计文档 §6.1）：物理网卡角色**必须按「永久 GUID / 描述关键字 / 名字」识别，
//! 绝不能依赖会变化的接口索引**。
//!
//! 本模块定义：
//! - 匹配规则的数据结构与单卡命中判定（纯函数，可单测）；
//! - **角色仲裁 `resolve_roles`**：给定系统枚举出的全部网卡，为 WAN/LAN 各确定唯一一块。
//!   规则为「精确（GUID/名称）优先于模糊关键字，同强度多命中即判歧义报错」——
//!   避免服务在多个同名 Wi-Fi 时被迫「取枚举第一块」这类写死猜测。
//! 具体系统查询（如按 GUID 枚举网卡）由 service 层完成，注入 AdapterProbe 列表进来匹配。

use serde::{Deserialize, Serialize};

use crate::CoreError;

/// 网卡承担的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdapterRole {
    /// 外网：持有全系统唯一默认路由 0.0.0.0/0。
    #[default]
    Wan,
    /// 内网：承载内网前缀路由，不带默认网关。
    Lan,
}

impl AdapterRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            AdapterRole::Wan => "wan",
            AdapterRole::Lan => "lan",
        }
    }
}

impl std::fmt::Display for AdapterRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单个网卡的一种识别依据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", content = "value", rename_all = "lowercase")]
pub enum AdapterMatcher {
    /// 网络接口的永久 GUID（形如 `{xxxxxxxx-xxxx-...}`），最可靠。
    #[serde(rename = "guid")]
    Guid(String),
    /// 网卡描述包含的关键字，如 "Realtek"、"Intel(R) Wi-Fi"。
    #[serde(rename = "desc-contains")]
    DescContains(String),
    /// 网卡友好名称（可能随系统语言变化，仅作辅助/展示）。
    #[serde(rename = "name-eq")]
    NameEq(String),
}

/// 命中强度：精确规则（GUID / 名称全等）高于模糊关键字规则。
/// 供角色仲裁时「精确优先」使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchWeight {
    /// DescContains 模糊关键字命中。
    Fuzzy,
    /// Guid / NameEq 精确命中。
    Exact,
}

/// GUID 归一化：去首尾花括号 + 全小写。
/// 配置 / 系统返回的 GUID 可能有花括号、大小写不一致，统一后比较才可靠。
pub fn normalize_guid(s: &str) -> String {
    s.trim().trim_start_matches('{').trim_end_matches('}').to_lowercase()
}

impl AdapterMatcher {
    /// 单条规则对某网卡的命中判定：未命中 → `None`；命中 → 返回强度。
    /// `matches` 只判真假，本方法把强度带给上层做仲裁。
    pub fn weight(&self, probe: &AdapterProbe) -> Option<MatchWeight> {
        let hit = match self {
            AdapterMatcher::Guid(g) => probe
                .guid
                .as_deref()
                .is_some_and(|pg| normalize_guid(g) == normalize_guid(pg)),
            AdapterMatcher::DescContains(k) => probe
                .description
                .as_deref()
                .is_some_and(|d| d.to_lowercase().contains(&k.to_lowercase())),
            AdapterMatcher::NameEq(n) => probe
                .name
                .as_deref()
                .is_some_and(|name| n.eq_ignore_ascii_case(name)),
        };
        if hit {
            Some(match self {
                AdapterMatcher::Guid(_) | AdapterMatcher::NameEq(_) => MatchWeight::Exact,
                AdapterMatcher::DescContains(_) => MatchWeight::Fuzzy,
            })
        } else {
            None
        }
    }
}

/// 某个角色对应的全部识别规则（命中其一即匹配）。允许多条以覆盖多同名适配器等情形。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleMatcher {
    pub role: AdapterRole,
    #[serde(default)]
    pub matchers: Vec<AdapterMatcher>,
}

impl RoleMatcher {
    pub fn new(role: AdapterRole) -> Self {
        RoleMatcher { role, matchers: Vec::new() }
    }

    pub fn with_matcher(mut self, m: AdapterMatcher) -> Self {
        self.matchers.push(m);
        self
    }

    /// 规则集对某网卡的**最高**命中强度（取最精确的一条命中）；全部未命中返回 `None`。
    pub fn max_weight(&self, probe: &AdapterProbe) -> Option<MatchWeight> {
        self.matchers.iter().filter_map(|m| m.weight(probe)).max()
    }

    /// 判断某网卡（guid / 描述 / 名字）是否命中本角色任一条规则。
    pub fn matches(&self, probe: &AdapterProbe) -> bool {
        self.max_weight(probe).is_some()
    }

    /// 供报错 / 诊断展示：规则的人类可读描述。
    fn describe(&self) -> String {
        if self.matchers.is_empty() {
            // 中性标（跨语言数据兜底，非 UI 文案）
            return "(no rules)".into();
        }
        let parts = self.matchers.iter().map(|m| match m {
            AdapterMatcher::Guid(g) => format!("guid={g}"),
            AdapterMatcher::DescContains(k) => format!("desc~{k}"),
            AdapterMatcher::NameEq(n) => format!("name={n}"),
        });
        format!("[{}]", parts.collect::<Vec<_>>().join(", "))
    }
}

/// 待匹配网卡的可识别字段快照。由 service 层从系统查询后构造。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdapterProbe {
    pub guid: Option<String>,
    pub description: Option<String>,
    pub name: Option<String>,
}

impl AdapterProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// 人类可读标识（用于报错 / 诊断），优先 guid，其次名字，再其次描述。
    pub fn label(&self) -> String {
        self.guid
            .clone()
            .or_else(|| self.name.clone())
            .or_else(|| self.description.clone())
            .unwrap_or_else(|| "(unknown adapter)".into())
    }
}

/// 角色仲裁结果：WAN / LAN 各自唯一落到一块真实网卡上。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleResolution {
    pub wan: AdapterProbe,
    pub lan: AdapterProbe,
}

/// 从「系统当前枚举到的全部网卡」中，为 WAN / LAN 各确定唯一一块。
///
/// 仲裁规则（把「多同名 Wi-Fi 该选哪块」的决策固化到这里，避免写死取枚举第一块）：
/// 1. **精确优先**：GUID / NameEq 精确命中，总是压过 DescContains 模糊命中；
/// 2. 同一命中强度下仍有多块 → `AmbiguousRole`，提示改用永久 GUID / 唯一名称精确定位；
/// 3. 无人命中 → `NoAdapterMatched`；WAN/LAN 落到同一块 → `Config` 错误。
pub fn resolve_roles(
    wan_matcher: &RoleMatcher,
    lan_matcher: &RoleMatcher,
    candidates: &[AdapterProbe],
) -> Result<RoleResolution, CoreError> {
    let wan = pick_role(AdapterRole::Wan, wan_matcher, candidates)?;
    let lan = pick_role(AdapterRole::Lan, lan_matcher, candidates)?;
    if same_adapter(wan, lan) {
        return Err(CoreError::Msg(crate::msgref!("DNERR", 52;)));
    }
    Ok(RoleResolution { wan: wan.clone(), lan: lan.clone() })
}

/// 为**单个角色**在一批候选网卡里解析唯一命中（另一侧为“空=不管理”时，diff 用本函数单独解析一侧）。
pub fn resolve_role<'a>(
    role: AdapterRole,
    matcher: &RoleMatcher,
    candidates: &'a [AdapterProbe],
) -> Result<&'a AdapterProbe, CoreError> {
    pick_role(role, matcher, candidates)
}

/// 为一个角色在候选网卡里选唯一命中：先比强度（精确优先），同强度并列多块 → 歧义报错。
fn pick_role<'a>(
    role: AdapterRole,
    rm: &RoleMatcher,
    candidates: &'a [AdapterProbe],
) -> Result<&'a AdapterProbe, CoreError> {
    let mut best_weight: Option<MatchWeight> = None;
    let mut best: Vec<&'a AdapterProbe> = Vec::new();
    for c in candidates {
        let Some(w) = rm.max_weight(c) else { continue };
        match best_weight {
            None => {
                best_weight = Some(w);
                best = vec![c];
            }
            Some(bw) if w > bw => {
                best_weight = Some(w);
                best = vec![c];
            }
            Some(bw) if w == bw => best.push(c),
            Some(_) => {}
        }
    }
    match best.len() {
        0 => Err(CoreError::NoAdapterMatched(role, rm.describe())),
        1 => Ok(best[0]),
        n => {
            // 块数进模板（DNERR-004 &2），列表只剩网卡标识（locale 无关数据）
            let list = best.iter().map(|p| p.label()).collect::<Vec<_>>().join("；");
            Err(CoreError::AmbiguousRole(role, n, list))
        }
    }
}

/// 两块网卡快照是否指向同一物理卡：guid 精确优先，其次友好名。
fn same_adapter(a: &AdapterProbe, b: &AdapterProbe) -> bool {
    match (a.guid.as_deref(), b.guid.as_deref()) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        _ => match (a.name.as_deref(), b.name.as_deref()) {
            (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
            _ => false,
        },
    }
}
