//! `dualnic-core` —— 双网卡分流核心逻辑层。
//!
//! 职责边界（与 `service` / `gui` 分离）：
//! - 纯逻辑、无 Windows 平台依赖、可在任意平台单测；
//! - 定义「期望路由意图」模型：把「WLAN=外网、以太网=内网」这类角色配置，
//!   翻译成「全局一条默认路由 + 内网前缀路由」的确定性目标态；
//! - 网卡角色识别规则（按 GUID / 描述关键字 / 名字，而非会变的接口索引）；
//! - 最长前缀匹配（LPM）可视化查询：输入任意 IPv4，输出将走哪块卡。
//!
//! service 负责从真实系统读取状态、把意图写回路由表并对账自愈；
//! 这里只定义数据结构与「意图 vs 实际」的判定逻辑。

pub mod config;
pub mod diff;
pub mod env;
pub mod intent;
pub mod ip;
pub mod ipc;
pub mod lpm;
pub mod msg;
pub mod role;

pub use config::PolicyConfig;
pub use intent::RouteIntent;
pub use role::{resolve_roles, AdapterRole, AdapterProbe, RoleMatcher, RoleResolution};

/// 各模块共享的错误类型，向上统一为 `anyhow` 亦可。
///
/// Display 会走消息目录渲染（`msg::t`）：GUI 进程按界面语言显示、服务进程按其安装的
/// 语言（SC，日志保持中文）渲染；`message()` 返回结构化引用供 IPC 跨语言传递。
#[derive(Debug)]
pub enum CoreError {
    /// CIDR / 地址解析失败（载荷为原始串）。
    InvalidAddress(String),
    MissingRole(AdapterRole),
    NoAdapterMatched(AdapterRole, String),
    AmbiguousRole(AdapterRole, usize, String),
    /// 配置校验失败（载荷为已渲染文本；新增错误点应优先用 `Msg` 携带结构化引用）。
    Config(String),
    /// 结构化错误消息（错误链消息化后的标准形态）。
    Msg(crate::msg::MessageRef),
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::InvalidAddress(s) => {
                write!(f, "{}", crate::msg::t(&crate::msgref!("DNERR", 1; s)))
            }
            CoreError::MissingRole(r) => {
                write!(f, "{}", crate::msg::t(&crate::msgref!("DNERR", 2; r.as_str())))
            }
            CoreError::NoAdapterMatched(r, rules) => {
                write!(f, "{}", crate::msg::t(&crate::msgref!("DNERR", 3; r.as_str(), rules)))
            }
            CoreError::AmbiguousRole(r, n, list) => {
                write!(
                    f,
                    "{}",
                    crate::msg::t(&crate::msgref!("DNERR", 4; r.as_str(), &n.to_string(), list))
                )
            }
            CoreError::Config(s) => {
                write!(f, "{}", crate::msg::t(&crate::msgref!("DNERR", 5; s)))
            }
            CoreError::Msg(m) => write!(f, "{}", crate::msg::t(m)),
        }
    }
}

impl std::error::Error for CoreError {}

impl CoreError {
    /// 结构化消息引用（IPC 跨语言传递用）：GUI 收到后按**自己的**语言渲染。
    pub fn message(&self) -> crate::msg::MessageRef {
        match self {
            CoreError::InvalidAddress(s) => crate::msgref!("DNERR", 1; s),
            CoreError::MissingRole(r) => crate::msgref!("DNERR", 2; r.as_str()),
            CoreError::NoAdapterMatched(r, rules) => {
                crate::msgref!("DNERR", 3; r.as_str(), rules)
            }
            CoreError::AmbiguousRole(r, n, list) => {
                crate::msgref!("DNERR", 4; r.as_str(), &n.to_string(), list)
            }
            CoreError::Config(s) => crate::msgref!("DNERR", 5; s),
            CoreError::Msg(m) => m.clone(),
        }
    }
}
