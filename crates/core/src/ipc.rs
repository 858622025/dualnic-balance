//! IPC 协议契约：本机 127.0.0.1 回环 TCP + **JSON 行**（自定义，非 HTTP）。
//!
//! 职责边界：本模块只定义
//! - 协议常量（版本 / 默认监听地址 / 单行上限）；
//! - serde 报文 DTO（请求 / 响应信封 / 状态载荷）；
//! - 作用在 `std::io` 抽象上的 framing（读一行 / 编码一行 / 解析请求）。
//!
//! **不出现任何 socket 类型、无 Windows 依赖、可单测** —— 监听与连接由
//! `dualnic-service` / `dualnic-gui` 各自接线，两端都引用这里的契约，保证同源。
//! 每次连接 = 一请求一响应后关闭（server 主动 close），规避半开/粘包/长连清理。

use std::io::{BufRead, ErrorKind};
use std::io;

use serde::{Deserialize, Serialize};

use crate::config::PolicyConfig;

/// 协议版本。响应信封的 `v` 字段；客户端收到更高版本应先提示升级而非硬解析。
pub const IPC_PROTOCOL_VERSION: u32 = 1;
/// 服务默认监听地址（回环固定端口）。可由 `--console --listen` / 未来配置覆盖。
pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:44175";
/// 单条 JSON 行长度上限，防内存放大。
pub const MAX_LINE_BYTES: usize = 16 * 1024;

// ──────────────────────────── 请求 ────────────────────────────

/// 客户端 → 服务端的请求。内部标签（`{"type":"..."}`），
/// 未知 type 会被 serde 判为非法 JSON → 服务端回 `bad_request`，天然前向兼容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// 存活探测：`{"type":"ping"}`。
    Ping,
    /// 取服务状态概要：`{"type":"get_status"}`。
    GetStatus,
    /// 取只读网络快照 + 意图 diff：`{"type":"get_snapshot"}`。
    GetSnapshot,
    /// 拿一次性会话令牌（保存配置前调用）：`{"type":"handshake"}`。
    Handshake,
    /// 取当前生效的完整配置（读不鉴权）：`{"type":"get_config"}`。
    GetConfig,
    /// 提交新配置（写盘 + 热重载）：`{"type":"set_config","token":"...","config":{...}}`。
    SetConfig {
        token: String,
        config: serde_json::Value,
    },
    /// 列出所有配置方案：`{"type":"list_profiles"}`。
    ListProfiles,
    /// 创建方案（from=None→默认配置，Some→复制）：`{"type":"create_profile","token":"...","name":"...","from":...}`。
    CreateProfile {
        token: String,
        name: String,
        from: Option<String>,
    },
    /// 删除方案：`{"type":"delete_profile","token":"...","name":"..."}`。
    DeleteProfile { token: String, name: String },
    /// 切换激活方案：`{"type":"switch_profile","token":"...","name":"..."}`。
    SwitchProfile { token: String, name: String },
    /// 重命名方案：`{"type":"rename_profile","token":"...","old_name":"...","new_name":"..."}`。
    RenameProfile {
        token: String,
        old_name: String,
        new_name: String,
    },
    /// 检测当前环境与各方案匹配情况（只读）：`{"type":"detect_environment"}`。
    DetectEnvironment,
    /// 自动匹配并应用（写）：`{"type":"auto_match","token":"..."}`。
    AutoMatch { token: String },
    /// 手动一键收敛（写路由）：`{"type":"reconcile_now","token":"..."}`。
    ReconcileNow { token: String },
    /// 一键诊断（只读）：`{"type":"diagnose"}`。返回系统路由/网卡原始命令输出 + 意图 vs 实际 diff。
    Diagnose,
    /// 暂停/恢复对账（写）：`{"type":"set_paused","token":"...","paused":true}`。暂停后手动/自动对账均不写路由。
    SetPaused { token: String, paused: bool },
    /// 取事件日志（只读）：`{"type":"get_events","since_unix_secs":...,"limit":...}`。
    GetEvents { since_unix_secs: Option<u64>, limit: usize },
    /// 导出配置（只读）：返回整个配置容器（ProfilesDoc）的 JSON 文本。
    ExportConfig,
    /// 清空全部事件日志（写）：`{"type":"clear_events","token":"..."}`。DELETE + VACUUM 回收文件空间。
    ClearEvents { token: String },
    /// 导入配置（写）：把 JSON 文本写回（解析 + 校验 + 原子写 + 热重载）。
    ImportConfig { token: String, config_json: String },
    /// 取全局「物理网卡」标记集合（只读）：`{"type":"get_physical_nics"}`。
    GetPhysicalNics,
    /// 设置全局「物理网卡」标记集合（写）：`{"type":"set_physical_nics","token":"...","guids":[...]}`。
    SetPhysicalNics { token: String, guids: Vec<String> },
    /// 读指定方案的配置（只读，编辑预览用）：`{"type":"get_profile_config","name":"..."}`。
    GetProfileConfig { name: String },
    /// 保存指定方案的配置（写，**不触发对账**——生效由 switch_profile 负责）：
    /// `{"type":"set_profile_config","token":"...","name":"...","config":{...}}`。
    SetProfileConfig { token: String, name: String, config: serde_json::Value },
}

/// 稳定错误码（`RpcError.code`）。服务端与 GUI 共享，避免魔法串。
pub mod error_code {
    /// 未知 type / 坏 JSON（既有）。
    pub const BAD_REQUEST: &str = "bad_request";
    /// 令牌缺失 / 不匹配 / 过期。
    pub const UNAUTHORIZED: &str = "unauthorized";
    /// 配置结构 / schema 版本 / validate 失败。
    pub const BAD_CONFIG: &str = "bad_config";
    /// 写盘 / 备份失败。
    pub const IO_FAILED: &str = "io_failed";
    /// 其它内部错误。
    pub const INTERNAL: &str = "internal";
    /// 方案不存在。
    pub const PROFILE_NOT_FOUND: &str = "profile_not_found";
    /// 方案名已存在。
    pub const ALREADY_EXISTS: &str = "already_exists";
    /// 写路由无管理员权限（ACCESS_DENIED）。
    pub const PERMISSION_DENIED: &str = "permission_denied";
}

/// Handshake 应答载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandshakeData {
    /// 一次性会话令牌（明文；仅进程内存）。
    pub token: String,
    /// 令牌有效期（秒）。
    #[serde(default)]
    pub ttl_secs: u32,
}

// ──────────────────────────── 响应信封 ────────────────────────────

/// 服务端 → 客户端的统一响应信封。
///
/// ```json
/// { "v":1, "ok":true,  "data":{ ... } }
/// { "v":1, "ok":false, "error":{ "code":"...", "message":"..." } }
/// ```
/// `data` 的具体形状由请求类型决定（Ping → `PingData`，GetStatus → `StatusData`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// 协议版本。
    pub v: u32,
    /// 是否成功。
    pub ok: bool,
    /// 失败原因（`ok=false` 时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    /// 成功载荷（`ok=true` 时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Response {
    pub fn ok(data: serde_json::Value) -> Self {
        Response { v: IPC_PROTOCOL_VERSION, ok: true, error: None, data: Some(data) }
    }
    pub fn err(error: RpcError) -> Self {
        Response { v: IPC_PROTOCOL_VERSION, ok: false, error: Some(error), data: None }
    }
}

/// 错误体：`code` 稳定（客户端可 switch）；`msg` 结构化引用由 GUI 按界面语言渲染
/// （服务端语言不跟随），`message` 为无结构化引用时的兜底纯文本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<crate::msg::MessageRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl RpcError {
    /// 标准构造：结构化消息（GUI 按界面语言渲染）。
    pub fn new(code: impl Into<String>, msg: crate::msg::MessageRef) -> Self {
        RpcError { code: code.into(), msg: Some(msg), message: None }
    }
    /// 兜底构造：无结构化引用的纯文本。
    pub fn text(code: impl Into<String>, message: impl Into<String>) -> Self {
        RpcError { code: code.into(), msg: None, message: Some(message.into()) }
    }
    /// 渲染为本地文本（优先结构化引用，按**当前进程**已安装语言）。
    pub fn render(&self) -> String {
        match (&self.msg, &self.message) {
            (Some(m), _) => crate::msg::t(m),
            (None, Some(s)) => s.clone(),
            (None, None) => self.code.clone(),
        }
    }
}

impl From<&IpcError> for RpcError {
    fn from(e: &IpcError) -> Self {
        match e {
            IpcError::LineTooLong { max } => {
                RpcError::new(e.code(), crate::msgref!("DNERR", 60; &max.to_string()))
            }
            IpcError::Read(err) => {
                RpcError::new(e.code(), crate::msgref!("DNERR", 61; &err.to_string()))
            }
            IpcError::Json(s) => RpcError::new(e.code(), crate::msgref!("DNERR", 62; s)),
        }
    }
}

// ──────────────────────────── 载荷 ────────────────────────────

/// Ping 应答。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PingData {
    pub pong: bool,
    /// 服务端 unix 秒。
    pub server_unix_secs: u64,
}

/// GetStatus 应答：服务运行概要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusData {
    pub protocol_version: u32,
    /// 固定 "running"（服务在线即 running）。
    pub service: String,
    pub pid: u32,
    /// 服务进程启动的 unix 秒。
    pub start_unix_secs: u64,
    /// 实际绑定的监听地址（含真实端口）。
    pub listen_addr: String,
    /// 配置加载概要。
    pub config: ConfigSummary,
    /// 路由风险概要（`None` = 服务端版本过旧未提供，不等于“无风险”）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<RiskSummary>,
    /// 对账是否被暂停（托盘「暂停分流」置位；暂停时手动/自动对账均不写路由）。
    #[serde(default)]
    pub paused: bool,
    /// 当前激活方案名。GUI 据此发现「后台切了方案」（自动匹配/IPC 切换）并重灌编辑缓冲。
    #[serde(default)]
    pub active_profile: String,
}

/// 配置加载概要（服务启动时一次加载；对账逻辑未启用前仅作状态展示）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSummary {
    /// 是否从磁盘成功加载（文件存在且 schema_version 支持且通过校验）。
    pub loaded: bool,
    /// 配置文件绝对路径。
    pub source_path: Option<String>,
    /// `loaded=false` 时的原因（文件缺失 / schema 过新 / 校验失败）。
    pub load_error: Option<crate::msg::MessageRef>,
    pub schema_version: u32,
    pub lan_networks_count: usize,
    /// WAN 角色是否已配置匹配规则。
    pub wan_rule_present: bool,
    /// LAN 角色是否已配置匹配规则。
    pub lan_rule_present: bool,
    pub reconciliation: ReconciliationSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationSummary {
    /// 收敛/清理是否任一开启（GUI「风险页」据此提示「未启用对账」）。
    pub enabled: bool,
    pub converge_default_route: bool,
    pub remove_stale_defaults: bool,
    pub protected_count: usize,
}

impl ConfigSummary {
    /// 从纯配置推导概要；`loaded / source_path / load_error` 三个运行时字段
    /// 由 service 用 struct-update 覆盖（`..ConfigSummary::from_config(&cfg)`）。
    pub fn from_config(cfg: &PolicyConfig) -> Self {
        let rec = &cfg.reconciliation;
        ConfigSummary {
            loaded: false,
            source_path: None,
            load_error: None,
            schema_version: cfg.schema_version,
            lan_networks_count: cfg.lan_networks.len(),
            wan_rule_present: !cfg.wan_adapter.matchers.is_empty(),
            lan_rule_present: !cfg.lan_adapter.matchers.is_empty(),
            reconciliation: ReconciliationSummary {
                enabled: rec.converge_default_route || rec.remove_stale_defaults,
                converge_default_route: rec.converge_default_route,
                remove_stale_defaults: rec.remove_stale_defaults,
                protected_count: rec.protected_interfaces.len(),
            },
        }
    }
}

// ──────────────────────────── 路由风险（风险页数据源） ────────────────────────────

/// IPv4 默认路由风险概要。判定唯一出口：`RiskLevel::assess(count, read_failed)`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskSummary {
    /// 系统当前所有 IPv4 默认路由（0.0.0.0/0，按 metric/接口排序）。
    #[serde(default)]
    pub default_routes: Vec<DefaultRouteRow>,
    /// 读取失败原因（此时 `default_routes` 为空）；成功为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_error: Option<crate::msg::MessageRef>,
}

impl RiskSummary {
    pub fn ok(routes: Vec<DefaultRouteRow>) -> Self {
        RiskSummary { default_routes: routes, read_error: None }
    }
    pub fn read_failed(msg: crate::msg::MessageRef) -> Self {
        RiskSummary { default_routes: Vec::new(), read_error: Some(msg) }
    }

    /// 据此概要的风险等级（判定规则唯一入口）。
    pub fn level(&self) -> RiskLevel {
        RiskLevel::assess(self.default_routes.len(), self.read_error.is_some())
    }
}

/// 一条 IPv4 默认路由。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultRouteRow {
    pub if_index: u32,
    /// 网卡描述（如 "Realtek ... Family Controller"，来自 GetIfTable2）。
    pub interface_desc: String,
    /// 连接名（如 "以太网" / "vEthernet (Default Switch)"）。
    pub interface_alias: String,
    /// 网关点分 IPv4（NextHop）。
    pub gateway: String,
    /// 路由 metric（dwForwardMetric1，Vista 起已含接口 metric）。
    pub metric: u32,
    /// 路由来源协议（dwForwardProto：3=netmgmt 静态/DHCP、19=autostatic 等，后续研判备用）。
    pub source_proto: u32,
}

/// 风险三态。规则唯一入口：读取失败→Yellow；≥2 条默认路由→Red；其余 Green。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    #[default]
    Green,
    Yellow,
    Red,
}

impl RiskLevel {
    pub fn assess(default_route_count: usize, read_failed: bool) -> RiskLevel {
        if read_failed {
            RiskLevel::Yellow
        } else if default_route_count >= 2 {
            RiskLevel::Red
        } else {
            RiskLevel::Green
        }
    }
}

// ──────────────────────────── 网络快照（GetSnapshot） ────────────────────────────

/// GetSnapshot 应答：只读网络快照 + 意图 vs 实际 diff。
/// 与 GetStatus 分离：只在 GUI 打开快照页/点刷新时取，不进 2s 轮询。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotData {
    /// 取数时刻（unix 秒）。
    pub fetched_at_unix_secs: u64,
    /// 读取失败说明（GUI 黄条）；None=成功。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_error: Option<crate::msg::MessageRef>,
    /// 角色解析 + diff 报告（类型在 core::diff，纯数据）。
    pub report: crate::diff::DiffReport,
}

// ──────────────────────────── 一键诊断（Diagnose） ────────────────────────────

/// 一键诊断应答：系统路由/网卡原始命令输出 + 意图 vs 实际 diff，供 GUI 展示与导出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnoseData {
    /// 取数时刻（unix 秒）。
    pub fetched_at_unix_secs: u64,
    /// 意图 vs 实际 diff（结构化，供 GUI 高亮）。
    pub report: crate::diff::DiffReport,
    /// `route print -4` 原始输出行。
    #[serde(default)]
    pub route_print: Vec<String>,
    /// `netsh interface ipv4 show interfaces` 原始输出行。
    #[serde(default)]
    pub net_interfaces: Vec<String>,
    /// `netsh interface ipv4 show config` 原始输出行。
    #[serde(default)]
    pub net_config: Vec<String>,
    /// 任一命令执行失败时的说明；None=全部成功。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_error: Option<crate::msg::MessageRef>,
}

impl DiagnoseData {
    /// 组合成一段可导出的纯文本报告（GUI「导出」按钮直接落盘）。
    pub fn to_text(&self) -> String {
        // 报告文案走全局消息运行时（GUI 启动时按所选语言 install）→ 导出语言跟随 GUI
        let mut s = String::new();
        s.push_str(&format!(
            "==== {} ====\n",
            crate::msg::t(&crate::msgref!("DNRPT", 1;))
        ));
        s.push_str(&format!(
            "{}\n\n",
            crate::msg::t(&crate::msgref!("DNRPT", 2; &self.fetched_at_unix_secs))
        ));
        if let Some(e) = &self.command_error {
            s.push_str(&format!(
                "{}\n\n",
                crate::msg::t(&crate::msgref!("DNRPT", 3; &crate::msg::t(e)))
            ));
        }
        s.push_str("==== route print -4 ====\n");
        for l in &self.route_print {
            s.push_str(l);
            s.push('\n');
        }
        s.push_str("\n==== netsh interface ipv4 show interfaces ====\n");
        for l in &self.net_interfaces {
            s.push_str(l);
            s.push('\n');
        }
        s.push_str("\n==== netsh interface ipv4 show config ====\n");
        for l in &self.net_config {
            s.push_str(l);
            s.push('\n');
        }
        s
    }
}

// ──────────────────────────── 事件日志（EventLog） ────────────────────────────

/// 一条结构化事件（服务端写入 SQLite，供 GUI 事件日志页展示）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventLogEntry {
    /// 事件时间（unix 秒）。
    pub ts_unix_secs: u64,
    /// 级别："info" | "warn" | "error"。
    pub level: String,
    /// 来源："reconcile" | "watch" | "config" | "auth" | "diagnose" | ...
    pub source: String,
    /// 消息：结构化字段缺失时的原文（老事件/老服务），语言无关的代码形态。
    pub message: String,
    /// v2 结构化消息（msgid/msgno/args）：非 None 时 GUI 按当前语言渲染。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<crate::msg::MessageRef>,
}

/// GetEvents 应答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsData {
    /// 按时间倒序返回的最近事件。
    pub entries: Vec<EventLogEntry>,
    /// 数据库文件大小（字节）。None = 内存降级库，无文件可量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_size_bytes: Option<u64>,
}

/// ClearEvents 应答：清空后的数据库大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearEventsData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_size_bytes: Option<u64>,
}

/// ExportConfig 应答：整个配置容器的 JSON 文本（None = 尚无配置）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportConfigData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_json: Option<String>,
}

/// GetPhysicalNics 应答：全局「物理网卡」标记集合（归一化 GUID）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalNicsData {
    #[serde(default)]
    pub guids: Vec<String>,
}

// ──────────────────────────── 配置方案（Profiles） ────────────────────────────

/// 方案列表里的单条（GUI 下拉）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileListEntry {
    pub name: String,
    pub active: bool,
    pub lan_networks_count: usize,
    pub wan_rule_present: bool,
    pub lan_rule_present: bool,
}

/// ListProfiles 应答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileListData {
    pub active: String,
    pub profiles: Vec<ProfileListEntry>,
}

// ──────────────────────────── 环境匹配（DetectEnvironment / AutoMatch） ────────────────────────────

/// 某个方案的匹配结论（供 GUI 展示）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvCandidate {
    pub name: String,
    pub matched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<crate::msg::MessageRef>,
}

/// DetectEnvironment 应答：当前环境指纹 + 各方案匹配结论。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvMatchReport {
    pub current_fingerprint: crate::env::EnvFingerprint,
    /// 恰好 1 个方案匹配时填其名；0 个或多个 = None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_profile: Option<String>,
    pub candidates: Vec<EnvCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_error: Option<crate::msg::MessageRef>,
}

/// AutoMatch 应答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoMatchData {
    /// "switched" | "created" | "noop"
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub summary: ConfigSummary,
}

/// 对账单步结果（供 GUI 展示）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileStep {
    pub op: String,    // "delete_default" | "delete_static_prefix" | "add_prefix" | "set_metric" | "skip_protected"
    pub target: String,
    pub detail: crate::msg::MessageRef,
}

/// 对账/回滚结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileResult {
    pub ok: bool,
    pub removed_defaults: u32,
    pub removed_static_prefixes: u32,
    pub added_prefixes: u32,
    pub metrics_fixed: u32,
    #[serde(default)]
    pub steps: Vec<ReconcileStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::msg::MessageRef>,
}

impl ReconcileResult {
    pub fn empty() -> Self {
        ReconcileResult {
            ok: true,
            removed_defaults: 0,
            removed_static_prefixes: 0,
            added_prefixes: 0,
            metrics_fixed: 0,
            steps: Vec::new(),
            error: None,
        }
    }
}

// ──────────────────────────── framing（纯函数，可单测） ────────────────────────────

/// 编码一行：序列化 + 补 `\n`。
pub fn encode_line<T: Serialize>(v: &T) -> Result<Vec<u8>, serde_json::Error> {
    let mut out = serde_json::to_vec(v)?;
    out.push(b'\n');
    Ok(out)
}

/// IPC 协议错误。Display 走消息目录（GUI 进程=界面语言，服务进程=SC）。
#[derive(Debug)]
pub enum IpcError {
    LineTooLong { max: usize },
    Read(io::Error),
    Json(String),
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcError::LineTooLong { max } => {
                write!(f, "{}", crate::msg::t(&crate::msgref!("DNERR", 60; &max.to_string())))
            }
            IpcError::Read(e) => {
                write!(f, "{}", crate::msg::t(&crate::msgref!("DNERR", 61; &e.to_string())))
            }
            IpcError::Json(s) => {
                write!(f, "{}", crate::msg::t(&crate::msgref!("DNERR", 62; s)))
            }
        }
    }
}

impl std::error::Error for IpcError {}

impl IpcError {
    /// 稳定错误码（放入 `RpcError.code`，客户端可 switch）。
    pub fn code(&self) -> &'static str {
        match self {
            IpcError::LineTooLong { .. } => "line_too_long",
            IpcError::Read(_) => "read_error",
            IpcError::Json(_) => "bad_request",
        }
    }
}

/// 从 `BufRead` 读一整行（以 `\n` 结尾，自动处理 TCP 半包/粘包），
/// 返回**不含**结尾 `\n`/`\r` 的字节；超过 `max` 上限 → `LineTooLong`；
/// 连接在对端写完整行前关闭 → `Read(UnexpectedEof)`。
pub fn read_line_bounded<R: BufRead>(r: &mut R, max: usize) -> Result<Vec<u8>, IpcError> {
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    loop {
        let chunk = r.fill_buf().map_err(IpcError::Read)?;
        if chunk.is_empty() {
            return Err(IpcError::Read(io::Error::new(
                ErrorKind::UnexpectedEof,
                crate::msg::t(&crate::msgref!("DNERR", 63;)),
            )));
        }
        let mut take = 0;
        let mut done = false;
        for &b in chunk {
            take += 1;
            buf.push(b);
            if b == b'\n' {
                done = true;
                break;
            }
        }
        r.consume(take);
        if done {
            while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
                buf.pop();
            }
            return Ok(buf);
        }
        if buf.len() > max {
            return Err(IpcError::LineTooLong { max });
        }
    }
}

/// 解析一整行请求字节为 `Request`。
pub fn parse_request(bytes: &[u8]) -> Result<Request, IpcError> {
    serde_json::from_slice(bytes).map_err(|e| IpcError::Json(e.to_string()))
}
