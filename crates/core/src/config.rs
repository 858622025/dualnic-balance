//! 策略配置模型：内网网段表 + 网卡角色映射。
//!
//! 配置文件（JSON / TOML，由 service 或 gui 读写）反序列化到 `PolicyConfig`。
//! 结构刻意设计为：**角色规则引用角色的网卡，而不是引用具体网卡名**——
//! 这样换机器 / 换网卡后，只需把网卡识别规则改对，策略内容（网段、metric）无需动。

use std::net::Ipv4Addr;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ip::Prefix;
use crate::role::{AdapterMatcher, AdapterRole, RoleMatcher};
use crate::CoreError;

/// 一条内网网段的策略（最终会落为一条前缀静态路由，走 Lan 网卡）。
/// 注意：含 Option<String>，故只派生 Clone 而非 Copy。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanNetwork {
    /// CIDR，如 "10.0.0.0/8"。
    pub cidr: Prefix,
    /// 可选的语义备注。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 手工网段绑定的网卡 GUID（归一化比较）。None = 随 LAN 卡 on-link 直连（自动网段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_iface_guid: Option<String>,
}

impl LanNetwork {
    pub fn new(cidr: Prefix, note: Option<&str>) -> Self {
        LanNetwork { cidr, note: note.map(str::to_owned), via_iface_guid: None }
    }
    /// 手工网段：显式绑定某块网卡，其下一跳取该网卡的 DHCP 服务器。
    pub fn with_via(cidr: Prefix, note: Option<&str>, via_iface_guid: Option<String>) -> Self {
        LanNetwork { cidr, note: note.map(str::to_owned), via_iface_guid }
    }
}

/// 配置结构当前版本。读到更高版本时拒绝加载，避免静默误解新语义。
pub const SCHEMA_VERSION: u32 = 1;

/// 完整策略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    /// 配置结构版本；JSON 缺该字段时按 `SCHEMA_VERSION`（1）处理。
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// 哪些网段算「内网」，走 Lan 角色网卡。
    pub lan_networks: Vec<LanNetwork>,
    /// 外网角色网卡的识别规则（默认路由 0.0.0.0/0 挂它身上）。
    pub wan_adapter: RoleMatcher,
    /// 内网角色网卡的识别规则（前缀静态路由挂它身上，不带默认网关）。
    pub lan_adapter: RoleMatcher,
    /// 接口 metric 固化值，避免自动跃点数摇摆（数值由部署按环境调，见 `InterfaceMetric`）。
    pub interface_metric: InterfaceMetric,
    /// 向导「外网网卡」连通性探测的目标地址（默认苹果官网；仅向导判定偏好，服务端路由对账不使用）。
    #[serde(default = "default_probe_target")]
    pub probe_target: String,
    /// 对账收敛的作用域与豁免（服务执行阶段消费）。
    pub reconciliation: Reconciliation,
    /// 方案「适用环境」快照（用于环境自动匹配；None=旧档/未记录）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<EnvironmentSnapshot>,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

fn default_probe_target() -> String {
    "www.apple.com".to_string()
}

/// 方案「适用环境」快照：每块已连接物理卡的「归一化 GUID + DHCP 服务器」组合。
/// 与 `env::EnvFingerprint` 的 adapters 对齐。绑定键只取 guid+DHCP 服务器：
/// DHCP 服务器是「我在哪个网」的稳定身份；网段会随 DHCP 重分配 / APIPA 瞬态变化，
/// 网段还有用户自定义的可能 —— 都不进匹配校验。
/// `None` = 旧档 / 未记录环境（自动匹配永不命中、永不自动切换/新建，退化为手动）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EnvironmentSnapshot {
    /// 每块已连接物理卡：归一化 GUID（去花括号+小写）+ DHCP 服务器。按 guid 排序保证可比。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adapters: Vec<AdapterEnv>,
}

/// environment 里的单块物理卡环境。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterEnv {
    /// 归一化 GUID（去花括号 + 小写）。
    pub guid: String,
    /// 该卡的 DHCP 服务器（静态 IP 卡为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcp_server: Option<Ipv4Addr>,
}

/// 对账收敛的作用域与豁免。整体默认「惰性」（不执行任何写操作），
/// 由部署侧在真实配置里显式开启 —— 保证 `PolicyConfig::default()`（无配置文件的首启）
/// 不会让服务自作主张改动系统路由。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reconciliation {
    /// 是否把默认路由收敛为「WAN 独占全系统唯一」。false = 纯只读观测（对账不写路由）。
    pub converge_default_route: bool,
    /// 非 WAN 且不在受保护列表的接口上出现默认路由时，视为过期并清除
    /// （针对其它物理卡 DHCP 注入的默认路由）。受保护接口不受本项影响。
    pub remove_stale_defaults: bool,
    /// 受保护接口（按永久 GUID）。对账对其**只读**：不增删改任何路由；
    /// 若其上出现默认路由（如 Hyper-V 共享 NAT、easetun 隧道），保留并记审计告警，**不擅自删除**。
    /// 防误删虚拟卡路由的安全兜底：部署时必须把 vEthernet / 隧道卡列入。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_interfaces: Vec<String>,
}

impl Default for Reconciliation {
    fn default() -> Self {
        Reconciliation {
            converge_default_route: false,
            remove_stale_defaults: false,
            protected_interfaces: Vec::new(),
        }
    }
}

/// 对账开关的产品级策略（前台不可配置）：**方案接管 WAN = 生效即收敛，两项强制全开**。
/// 未管理 WAN 的方案强制关闭 —— 没有 WAN 角色时「收敛默认路由」= 删光全系统默认路由，纯危险。
/// 由 GUI 保存与服务端唯一写入口（write_profiles_locked）共同调用，覆盖新旧存档与导入。
pub fn apply_reconciliation_policy(cfg: &mut PolicyConfig) {
    let manages_wan = !cfg.wan_adapter.matchers.is_empty();
    cfg.reconciliation.converge_default_route = manages_wan;
    cfg.reconciliation.remove_stale_defaults = manages_wan;
}

/// 接口 metric 固化值（默认 LAN 10 / WAN 50，仅占位，真实数值由部署按环境写配置）。
/// 注意：LAN 数值**并非必须低于 WAN** —— 默认路由收敛后 LAN 不再持有默认，
/// 两块卡的 metric 相对值取决于「接口出现竞态候选时的排序」，勿在代码里做任何顺序假设。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceMetric {
    pub lan: u32,
    pub wan: u32,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        PolicyConfig {
            schema_version: SCHEMA_VERSION,
            lan_networks: Vec::new(),
            wan_adapter: RoleMatcher::new(AdapterRole::Wan),
            lan_adapter: RoleMatcher::new(AdapterRole::Lan),
            interface_metric: InterfaceMetric::default(),
            probe_target: default_probe_target(),
            reconciliation: Reconciliation::default(),
            environment: None,
        }
    }
}

impl Default for InterfaceMetric {
    fn default() -> Self {
        // 默认值唯一出口：PolicyConfig::default 从这里取，避免两处手写漂移。
        InterfaceMetric { lan: 10, wan: 50 }
    }
}

impl PolicyConfig {
    /// 追加一条内网网段。
    pub fn add_lan(&mut self, cidr: &str) -> Result<(), CoreError> {
        let prefix: Prefix = cidr.parse()?;
        if prefix.len == 0 {
            return Err(CoreError::Msg(crate::msgref!("DNERR", 20;)));
        }
        if self.lan_networks.iter().any(|n| n.cidr == prefix) {
            return Ok(()); // 幂等
        }
        self.lan_networks.push(LanNetwork::new(prefix, None));
        Ok(())
    }

    /// 解析配置并做一致性校验。
    ///
    /// 角色规则可为空 —— **空 = 该侧不管理**（例如：只配内网前缀但不管外网默认路由、
    /// 或全空=纯观测不动路由）。此处只做**静态**预检：把「运行时就注定二义」的配置提前暴露；
    /// 依赖真实网卡数量的模糊规则重叠留给运行时 `resolve_roles` 判定。
    pub fn validate(&self) -> Result<(), CoreError> {
        // 收集某个角色里「精确取值」的**去重**小写集合（GUID 与友好名分开）。
        let exact_ids = |rm: &RoleMatcher| -> (Vec<String>, Vec<String>) {
            let mut guids: Vec<String> = rm
                .matchers
                .iter()
                .filter_map(|m| match m {
                    AdapterMatcher::Guid(g) => Some(crate::role::normalize_guid(g)),
                    _ => None,
                })
                .collect();
            guids.sort();
            guids.dedup();
            let mut names: Vec<String> = rm
                .matchers
                .iter()
                .filter_map(|m| match m {
                    AdapterMatcher::NameEq(n) => Some(n.to_lowercase()),
                    _ => None,
                })
                .collect();
            names.sort();
            names.dedup();
            (guids, names)
        };

        let (wan_guids, wan_names) = exact_ids(&self.wan_adapter);
        let (lan_guids, lan_names) = exact_ids(&self.lan_adapter);

        // 单角色内：多条不同 GUID / 名称 → 运行时任何时刻命中它们必然选中 ≥2 块（同强度并列）
        // → Ambiguous，配置层面即为非法。多条不同 desc 允许（模糊可由真实网卡数消解）。
        if wan_guids.len() > 1 || wan_names.len() > 1 {
return Err(CoreError::Msg(crate::msgref!("DNERR", 21; "WAN")));
        }
        if lan_guids.len() > 1 || lan_names.len() > 1 {
return Err(CoreError::Msg(crate::msgref!("DNERR", 21; "LAN")));
        }

        // 跨角色：GUID / 名称取值相同 → 必指向同一块网卡（会造成路由环）。
        let share = |a: &[String], b: &[String]| a.iter().any(|x| b.contains(x));
        if share(&wan_guids, &lan_guids) {
            return Err(CoreError::Msg(crate::msgref!("DNERR", 22;)));
        }
        if share(&wan_names, &lan_names) {
            return Err(CoreError::Msg(crate::msgref!("DNERR", 23;)));
        }
        Ok(())
    }

    /// 从 JSON 字符串加载。
    pub fn from_json_str(s: &str) -> Result<Self, CoreError> {
        let v: serde_json::Value = serde_json::from_str(s)
            .map_err(|e| CoreError::Msg(crate::msgref!("DNERR", 24; &e.to_string())))?;
        Self::from_json_value(v)
    }

    /// 从 serde Value 解析（SetConfig 服务端唯一闸门）：结构/schema 版本/validate 一条链。
    pub fn from_json_value(v: serde_json::Value) -> Result<Self, CoreError> {
        let cfg: PolicyConfig = serde_json::from_value(v)
            .map_err(|e| CoreError::Msg(crate::msgref!("DNERR", 24; &e.to_string())))?;
        if cfg.schema_version > SCHEMA_VERSION {
            return Err(CoreError::Msg(crate::msgref!(
                "DNERR", 25; &cfg.schema_version.to_string(), &SCHEMA_VERSION.to_string()
            )));
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// 从 JSON 文件加载（路径可不存在 → 用默认值）。方便 service 首启。
    pub fn load_or_default<P: AsRef<Path>>(p: P) -> Result<Self, CoreError> {
        let path = p.as_ref();
        if path.exists() {
            let s = std::fs::read_to_string(path)
                .map_err(|e| CoreError::Msg(crate::msgref!("DNERR", 26; &path.display().to_string(), &e.to_string())))?;
            Self::from_json_str(&s)
        } else {
            Ok(PolicyConfig::default())
        }
    }
}

/// 便捷构建器：演示「WAN=WLAN / LAN=以太网」最小可用配置。
/// 真实部署时由 GUI 向导或配置文件生成。
impl PolicyConfig {
    pub fn builder() -> PolicyConfigBuilder {
        PolicyConfigBuilder::default()
    }
}

#[derive(Default)]
pub struct PolicyConfigBuilder {
    inner: PolicyConfig,
}

impl PolicyConfigBuilder {
    pub fn wan_by_desc(mut self, keyword: &str) -> Self {
        self.inner.wan_adapter.matchers.push(AdapterMatcher::DescContains(keyword.to_owned()));
        self
    }
    pub fn lan_by_desc(mut self, keyword: &str) -> Self {
        self.inner.lan_adapter.matchers.push(AdapterMatcher::DescContains(keyword.to_owned()));
        self
    }
    pub fn lan_cidr(mut self, cidr: &str) -> Result<Self, CoreError> {
        self.inner.add_lan(cidr)?;
        Ok(self)
    }
    pub fn lan_host(mut self, ip: Ipv4Addr) -> Self {
        self.inner
            .lan_networks
            .push(LanNetwork::new(crate::ip::Prefix::host(ip), None));
        self
    }
    pub fn build(self) -> Result<PolicyConfig, CoreError> {
        self.inner.validate()?;
        Ok(self.inner)
    }
}

// ──────────────────────────── 多配置方案（Profiles） ────────────────────────────

/// 配置文件（容器）当前版本：2 = profile 文档。
pub const PROFILES_SCHEMA_VERSION: u32 = 2;
/// 默认 profile 名，用于旧单份文件迁移 / 首启种子。
pub const DEFAULT_PROFILE_NAME: &str = "默认";

/// 一个配置方案：名字 + 一份完整策略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// 全局唯一、非空（GUI 限 ≤48）；也作为 `active_profile` 引用键。
    pub name: String,
    pub config: PolicyConfig,
}

impl Profile {
    pub fn new(name: impl Into<String>, config: PolicyConfig) -> Self {
        Profile { name: name.into(), config }
    }
}

/// 配置文件容器：包含多组配置方案与当前激活的方案名。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfilesDoc {
    /// 文档版本（== PROFILES_SCHEMA_VERSION）；每 profile 内嵌 `config.schema_version` 仍为 1。
    #[serde(default = "default_profiles_schema_version")]
    pub schema_version: u32,
    /// 引用 `profiles[i].name`。
    #[serde(default = "default_profile_name")]
    pub active_profile: String,
    #[serde(default = "default_profiles")]
    pub profiles: Vec<Profile>,
    /// 用户标记的「物理网卡」GUID 集合（归一化）。**全局跨方案**；空 = 未标记 = 无物理卡。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub physical_nic_guids: Vec<String>,
}

fn default_profiles_schema_version() -> u32 {
    PROFILES_SCHEMA_VERSION
}
fn default_profile_name() -> String {
    DEFAULT_PROFILE_NAME.to_string()
}
fn default_profiles() -> Vec<Profile> {
    vec![Profile::new(DEFAULT_PROFILE_NAME, PolicyConfig::default())]
}

impl Default for ProfilesDoc {
    fn default() -> Self {
        ProfilesDoc {
            schema_version: PROFILES_SCHEMA_VERSION,
            active_profile: DEFAULT_PROFILE_NAME.to_string(),
            profiles: default_profiles(),
            physical_nic_guids: Vec::new(),
        }
    }
}

impl ProfilesDoc {
    /// 单一 name → Profile 引用（GUI 解析 active 用）。
    pub fn find(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// 当前激活的 profile 配置。
    pub fn active_config(&self) -> &PolicyConfig {
        let name = self.active_profile.as_str();
        self.find(name).map(|p| &p.config).unwrap_or_else(|| &self.profiles[0].config)
    }

    /// 从 JSON 解析并校验（唯一闸门）：文档 schema>2 拒；profiles 非空；每份 config 走
    /// `PolicyConfig::from_json_value`（内嵌 schema 校验 + validate）；name 非空且互不重复；
    /// active 必须存在于 profiles（缺失则回退到第一个并返回其名）。
    pub fn from_json_value(v: serde_json::Value) -> Result<(Self, Option<String>), CoreError> {
        let mut doc: ProfilesDoc = serde_json::from_value(v)
            .map_err(|e| CoreError::Msg(crate::msgref!("DNERR", 24; &e.to_string())))?;

        if doc.schema_version > PROFILES_SCHEMA_VERSION {
            return Err(CoreError::Msg(crate::msgref!(
                "DNERR", 25; &doc.schema_version.to_string(), &PROFILES_SCHEMA_VERSION.to_string()
            )));
        }
        if doc.profiles.is_empty() {
            return Err(CoreError::Msg(crate::msgref!("DNERR", 27;)));
        }
        // 每份 config 单独校验（复用 PolicyConfig::from_json_value 闸门）
        for p in &mut doc.profiles {
            if p.name.trim().is_empty() {
                return Err(CoreError::Msg(crate::msgref!("DNERR", 28;)));
            }
            if p.config.schema_version > SCHEMA_VERSION {
                return Err(CoreError::Msg(crate::msgref!(
                    "DNERR", 29; &p.name, &p.config.schema_version.to_string(), &SCHEMA_VERSION.to_string()
                )));
            }
            p.config.validate()?;
        }
        // profile 名互斥
        let mut seen = std::collections::HashSet::new();
        for p in &doc.profiles {
            if !seen.insert(p.name.clone()) {
                return Err(CoreError::Msg(crate::msgref!("DNERR", 30; &p.name)));
            }
        }
        // active 必须存在于 profiles；缺失回退到第一个
        let mut fallback = None;
        if !doc.profiles.iter().any(|p| p.name == doc.active_profile) {
            fallback = Some(doc.profiles[0].name.clone());
            doc.active_profile = fallback.clone().unwrap();
        }
        Ok((doc, fallback))
    }

    /// 从 JSON 字符串解析（见 from_json_value）。
    pub fn from_json_str(s: &str) -> Result<(Self, Option<String>), CoreError> {
        let v: serde_json::Value = serde_json::from_str(s)
            .map_err(|e| CoreError::Msg(crate::msgref!("DNERR", 24; &e.to_string())))?;
        Self::from_json_value(v)
    }
}

impl PolicyConfig {
    /// 演示/自测用示例配置：WAN=Intel Wi-Fi、LAN=Realtek，两条内网网段。
    /// 仅用于骨架演示（gui/service 的 demo 输出），真实配置由部署侧提供。
    pub fn builder_demo() -> Result<PolicyConfig, CoreError> {
        let mut cfg = Self::builder()
            .wan_by_desc("Intel Wi-Fi")
            .lan_by_desc("Realtek")
            .lan_cidr("10.0.0.0/8")?
            .lan_cidr("203.0.113.0/24")?
            .build()?;
        // demo 代表「真实部署画像」：开启默认路由收敛与过期默认清理；
        // 注意 demo 不配 protected_interfaces —— 真实部署必须把 vEthernet/隧道卡列入豁免。
        cfg.reconciliation = Reconciliation {
            converge_default_route: true,
            remove_stale_defaults: true,
            ..Reconciliation::default()
        };
        Ok(cfg)
    }
}
