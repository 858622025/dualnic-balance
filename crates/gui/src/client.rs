//! 与服务的单发 IPC 客户端（JSON 行协议，契约同源 `dualnic_core::ipc`）。
//!
//! 每次请求都新建连接 = 天然热重连：服务不可达 → `Err`（清晰文案），服务恢复 → 自动恢复。
//! 新增写/读配置方法返回 `RpcFailure{code,message}`，便于 GUI 按错误码分支（如令牌失效）。

use std::fmt;
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use dualnic_core::config::PolicyConfig;
use dualnic_core::ipc::{
    self, AutoMatchData, DiagnoseData, EnvMatchReport, EventsData, ExportConfigData, HandshakeData,
    ProfileListData, ReconcileResult, Request, Response, SnapshotData, StatusData,
};

const TIMEOUT: Duration = Duration::from_secs(3);

/// 一次 RPC 失败的稳定错误码 + 中文文案（code 见 `dualnic_core::ipc::error_code`）。
#[derive(Debug, Clone)]
pub struct RpcFailure {
    pub code: String,
    pub message: String,
}

impl fmt::Display for RpcFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RpcFailure {}

impl RpcFailure {
    fn connect(e: std::io::Error) -> Self {
        // GUI 进程已安装语言目录 → 构造点直接按界面语言渲染
        RpcFailure {
            code: "connect".into(),
            message: dualnic_core::msg::t(&dualnic_core::msgref!("DNERR", 120; &e.to_string())),
        }
    }
    /// 本地错误（结构化）：按界面语言渲染后装入。
    fn local_msg(m: dualnic_core::msg::MessageRef) -> Self {
        RpcFailure { code: "local".into(), message: dualnic_core::msg::t(&m) }
    }
}

/// 完成一次请求，返回 `data`（ok=true）；失败带 code。
pub fn call(req: &Request) -> Result<serde_json::Value, RpcFailure> {
    let mut sock = TcpStream::connect(ipc::DEFAULT_LISTEN_ADDR).map_err(RpcFailure::connect)?;
    sock.set_read_timeout(Some(TIMEOUT)).ok();
    sock.set_write_timeout(Some(TIMEOUT)).ok();

    let bytes = ipc::encode_line(req).map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 121; &e.to_string())))?;
    sock.write_all(&bytes).map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 122; &e.to_string())))?;

    let line = {
        let mut reader = BufReader::new(&sock);
        ipc::read_line_bounded(&mut reader, ipc::MAX_LINE_BYTES)
    }
    .map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 123; &e.to_string())))?;

    let resp: Response =
        serde_json::from_slice(&line).map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 124; &e.to_string())))?;
    if resp.v != ipc::IPC_PROTOCOL_VERSION {
        return Err(RpcFailure::local_msg(dualnic_core::msgref!(
            "DNERR", 125; &resp.v.to_string(), &ipc::IPC_PROTOCOL_VERSION.to_string()
        )));
    }
    if resp.ok {
        resp.data.ok_or_else(|| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 126;)))
    } else {
        let err = resp
            .error
            .unwrap_or_else(|| ipc::RpcError::text("unknown", dualnic_core::msg::t(&dualnic_core::msgref!("DNERR", 127;))));
        // 结构化错误按 GUI 界面语言渲染（服务端语言不跟随）
        let message = err.render();
        Err(RpcFailure { code: err.code, message })
    }
}

/// 兼容旧调用方：失败抹成 message 字符串。
pub fn request(req: &Request) -> Result<serde_json::Value, String> {
    call(req).map_err(|f| f.message)
}

/// 取服务状态。
pub fn get_status() -> Result<StatusData, String> {
    let data = request(&Request::GetStatus)?;
    serde_json::from_value(data).map_err(|e| dualnic_core::msg::t(&dualnic_core::msgref!("DNERR", 128; &e.to_string())))
}

/// 取只读网络快照 + 意图 diff（独立请求，仅快照页使用）。
pub fn get_snapshot() -> Result<SnapshotData, String> {
    let data = request(&Request::GetSnapshot)?;
    serde_json::from_value(data).map_err(|e| dualnic_core::msg::t(&dualnic_core::msgref!("DNERR", 129; &e.to_string())))
}

/// 一键诊断（只读）：系统路由/网卡原始命令输出 + 意图 diff。
pub fn diagnose() -> Result<DiagnoseData, String> {
    let data = request(&Request::Diagnose)?;
    serde_json::from_value(data).map_err(|e| dualnic_core::msg::t(&dualnic_core::msgref!("DNERR", 130; &e.to_string())))
}

/// 暂停/恢复对账（写，需令牌）。
pub fn set_paused(token: &str, paused: bool) -> Result<(), RpcFailure> {
    let _ = call(&Request::SetPaused { token: token.to_string(), paused })?;
    Ok(())
}

/// 取事件日志（只读）。
pub fn get_events(since: Option<u64>, limit: usize) -> Result<EventsData, String> {
    let data = request(&Request::GetEvents { since_unix_secs: since, limit })?;
    serde_json::from_value(data).map_err(|e| dualnic_core::msg::t(&dualnic_core::msgref!("DNERR", 131; &e.to_string())))
}

/// 清空全部事件日志（写，需令牌）。返回清空后的数据库大小。
pub fn clear_events(token: &str) -> Result<Option<u64>, RpcFailure> {
    let data = call(&Request::ClearEvents { token: token.to_string() })?;
    let d: ipc::ClearEventsData = serde_json::from_value(data)
        .map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 132; &e.to_string())))?;
    Ok(d.db_size_bytes)
}

/// 导出配置（只读）：整个配置容器的 JSON 文本（None = 尚无配置）。
pub fn export_config() -> Result<Option<String>, RpcFailure> {
    let data = call(&Request::ExportConfig)?;
    let d: ExportConfigData = serde_json::from_value(data)
        .map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 133; &e.to_string())))?;
    Ok(d.config_json)
}

/// 导入配置（写，需令牌）。
pub fn import_config(token: &str, config_json: &str) -> Result<(), RpcFailure> {
    let _ = call(&Request::ImportConfig {
        token: token.to_string(),
        config_json: config_json.to_string(),
    })?;
    Ok(())
}

/// 握手拿一次性会话令牌（保存配置前）。
pub fn handshake() -> Result<String, RpcFailure> {
    let data = call(&Request::Handshake)?;
    let hs: HandshakeData =
        serde_json::from_value(data).map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 134; &e.to_string())))?;
    Ok(hs.token)
}

// ──────────────────────────── 编辑/生效解耦：按方案读写 ────────────────────────────

/// 读指定方案的配置（编辑预览；与激活状态无关）。
pub fn get_profile_config(name: &str) -> Result<PolicyConfig, RpcFailure> {
    let data = call(&Request::GetProfileConfig { name: name.to_string() })?;
    PolicyConfig::from_json_value(data).map_err(|e| RpcFailure::local_msg(e.message()))
}

/// 保存指定方案的配置（只持久化，不触发对账；生效走 switch_profile）。
pub fn set_profile_config(token: &str, name: &str, cfg: &PolicyConfig) -> Result<(), RpcFailure> {
    let value = serde_json::to_value(cfg)
        .map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 135; &e.to_string())))?;
    let _ = call(&Request::SetProfileConfig {
        token: token.to_string(),
        name: name.to_string(),
        config: value,
    })?;
    Ok(())
}

// ──────────────────────────── 全局物理网卡标记 ────────────────────────────

/// 取全局「物理网卡」标记集合（归一化 GUID）。
pub fn get_physical_nics() -> Result<Vec<String>, RpcFailure> {
    let data = call(&Request::GetPhysicalNics)?;
    let d: dualnic_core::ipc::PhysicalNicsData =
        serde_json::from_value(data).map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 136; &e.to_string())))?;
    Ok(d.guids)
}

/// 写全局「物理网卡」标记集合。
pub fn set_physical_nics(token: &str, guids: &[String]) -> Result<(), RpcFailure> {
    let _ = call(&Request::SetPhysicalNics { token: token.to_string(), guids: guids.to_vec() })?;
    Ok(())
}

// ──────────────────────────── 配置方案（Profile） ────────────────────────────

pub fn list_profiles() -> Result<ProfileListData, RpcFailure> {
    let data = call(&Request::ListProfiles)?;
    serde_json::from_value(data).map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 137; &e.to_string())))
}


pub fn create_profile(token: &str, name: &str, from: Option<&str>) -> Result<(), RpcFailure> {
    let _ = call(&Request::CreateProfile {
        token: token.to_string(),
        name: name.to_string(),
        from: from.map(str::to_string),
    })?;
    Ok(())
}

pub fn delete_profile(token: &str, name: &str) -> Result<(), RpcFailure> {
    let _ = call(&Request::DeleteProfile { token: token.to_string(), name: name.to_string() })?;
    Ok(())
}

pub fn switch_profile(token: &str, name: &str) -> Result<(), RpcFailure> {
    let _ = call(&Request::SwitchProfile { token: token.to_string(), name: name.to_string() })?;
    Ok(())
}

pub fn rename_profile(token: &str, old: &str, new: &str) -> Result<(), RpcFailure> {
    let _ = call(&Request::RenameProfile {
        token: token.to_string(),
        old_name: old.to_string(),
        new_name: new.to_string(),
    })?;
    Ok(())
}

// ──────────────────────────── 环境匹配 ────────────────────────────

/// 检测当前环境与各方案匹配情况（只读）。
pub fn detect_environment() -> Result<EnvMatchReport, RpcFailure> {
    let data = call(&Request::DetectEnvironment)?;
    serde_json::from_value(data).map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 138; &e.to_string())))
}

/// 自动匹配并应用（写）。
pub fn auto_match(token: &str) -> Result<AutoMatchData, RpcFailure> {
    let data = call(&Request::AutoMatch { token: token.to_string() })?;
    serde_json::from_value(data).map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 139; &e.to_string())))
}

// ──────────────────────────── 对账（写路由） ────────────────────────────

/// 手动一键收敛。
pub fn reconcile_now(token: &str) -> Result<ReconcileResult, RpcFailure> {
    let data = call(&Request::ReconcileNow { token: token.to_string() })?;
    serde_json::from_value(data).map_err(|e| RpcFailure::local_msg(dualnic_core::msgref!("DNERR", 140; &e.to_string())))
}
