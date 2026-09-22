//! 配置 / 日志路径解析。
//!
//! 一律绝对路径、不依赖进程 cwd（SCM 以 LocalSystem 启动时 cwd 是 system32）。
//! 用 `%ProgramData%`（LocalSystem 可写、普通用户只读）而不用 %LOCALAPPDATA%。

use std::path::PathBuf;

fn program_data_dir() -> PathBuf {
    std::env::var_os("PROGRAMDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

/// 产品数据根目录。
pub fn base_dir() -> PathBuf {
    program_data_dir().join("DualNIC Balance")
}

/// 配置文件路径。可用环境变量 `DUALNIC_CONFIG_PATH` 覆盖（便于联调/测试）。
pub fn config_path() -> PathBuf {
    std::env::var_os("DUALNIC_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| base_dir().join("config.json"))
}

/// 服务模式日志目录。
pub fn log_dir() -> PathBuf {
    base_dir().join("logs")
}

/// 确保配置父目录与日志目录存在（幂等）。
pub fn ensure_dirs() -> std::io::Result<()> {
    if let Some(parent) = config_path().parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(log_dir())?;
    Ok(())
}

/// 单级备份文件（与 config 同目录，`config.json.bak`）。
/// SQLite 方案下配置存库内 `main` key；导出 JSON 直接走 `db.get_config_raw`。

/// 本工具路由产物清单（记录本工具添加的前缀路由，供切惰性方案时清理）。
pub fn route_manifest_path() -> PathBuf {
    base_dir().join("route-manifest.json")
}

/// SQLite 数据库文件：配置 + 事件日志统一存这里。
pub fn db_path() -> PathBuf {
    base_dir().join("dualnic.db")
}
