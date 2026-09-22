//! `dualnic-service` 入口 —— 常驻服务（服务壳 + IPC + 对账自愈）。
//!
//! 两种运行模式：
//! - **无参数**：作为 LocalSystem Windows 服务运行（由 SCM 拉起，`cfg(windows)`）。
//! - **`--console [--listen ADDR]`**：前台调试模式，无需管理员；Ctrl-C 优雅退出。
//! - **`--help`**：用法。
//!
//! 职责：加载配置（只读，惰性）+ 路由对账/自愈 + 环境自动匹配 + 127.0.0.1 JSON 行 IPC 服务。
//! 意图/LPM/角色仲裁等纯逻辑由 core 覆盖，服务负责接线与执行。

use std::process::ExitCode;
use std::sync::Arc;

mod auth;
mod db;
mod paths;
mod server;
mod status;

#[cfg(windows)]
mod net;
#[cfg(windows)]
mod diagnose;
#[cfg(windows)]
mod reconcile;
#[cfg(windows)]
mod scm;
#[cfg(windows)]
mod watch;

use dualnic_core::ipc::DEFAULT_LISTEN_ADDR;

fn main() -> ExitCode {
    // 消息渲染语言：服务进程固定 SC（决策：服务日志保持中文）。
    // GUI 收到的错误经 IPC 以 MessageRef 传递、由 GUI 按界面语言渲染，不受此处影响。
    // lang 目录缺失时回退出厂内嵌 SC 包，日志渲染不受部署形态影响。
    let lang_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("lang")));
    // 语言文件完整性告警先缓存，等 tracing 初始化后再落日志（SCM 模式无控制台）。
    let lang_warnings = dualnic_core::msg::install(lang_dir.as_deref(), "SC");

    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--console") {
        let listen = arg_value(&args, "--listen").unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_string());
        match run_console(&listen, &lang_warnings) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 1; &e.to_string())));
                ExitCode::FAILURE
            }
        }
    } else {
        run_service_mode(&lang_warnings)
    }
}

#[cfg(windows)]
fn run_service_mode(lang_warnings: &[String]) -> ExitCode {
    if let Err(e) = paths::ensure_dirs() {
        eprintln!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 2; &e.to_string())));
        return ExitCode::FAILURE;
    }
    // 服务模式下日志写文件（SCM 无控制台）；guard 存活到服务停止（dispatcher::start 阻塞期间）。
    let _guard = init_file_tracing();
    for w in lang_warnings {
        tracing::warn!("[i18n] {w}");
    }
    match scm::start_dispatcher() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // 典型：桌面双击运行而非被 SCM 拉起 → ERROR_FAILED_SERVICE_CONTROLLER_CONNECT(1063)
            eprintln!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 3; &e.to_string())));
            eprintln!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 4;)));
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn run_service_mode(lang_warnings: &[String]) -> ExitCode {
    for w in lang_warnings {
        eprintln!("[i18n] {w}");
    }
    eprintln!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 5;)));
    ExitCode::FAILURE
}

/// 前台调试：stdout 日志 + Ctrl-C 优雅退出，跑同一个 IPC server。
fn run_console(listen: &str, lang_warnings: &[String]) -> std::io::Result<()> {
    init_console_tracing();
    for w in lang_warnings {
        tracing::warn!("[i18n] {w}");
    }
    tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 6;)));

    let start_secs = status::unix_now_secs();
    let shutdown = server::Shutdown::new();
    let ctrlc_shutdown = shutdown.clone();
    if let Err(e) = ctrlc::set_handler(move || {
        tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 7;)));
        ctrlc_shutdown.request();
    }) {
        tracing::warn!(error = %e, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 8;)));
    }

    let (srv, addr) = server::IpcServer::bind(listen)?;
    let state = Arc::new(status::load_runtime(addr, start_secs));
    let loaded = status::config_snapshot(&state).loaded;
    tracing::info!(
        "{}",
        dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 9; &addr.to_string(), &loaded.to_string(), &state.config_path.display().to_string()))
    );
    tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 10;)));

    let auth = Arc::new(auth::SessionAuth::new());
    let serve_shutdown = shutdown.clone();
    let serve_state = Arc::clone(&state);
    let serve_auth = Arc::clone(&auth);
    let thread = std::thread::spawn(move || srv.serve_loop(&serve_state, &serve_auth, &serve_shutdown));

    // 环境自动匹配监听（网络事件 + 定时兜底），与 IPC 服务并行；共用同一 Shutdown 停止。
    #[cfg(windows)]
    crate::watch::start(Arc::clone(&state), shutdown.clone());

    shutdown.wait_stopped();
    tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 11;)));
    let _ = thread.join();
    tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 12;)));
    Ok(())
}

// ──────────────────────────── 日志接线 ────────────────────────────

fn default_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "dualnic_service=debug,dualnic_core=info".into())
}

fn init_console_tracing() {
    tracing_subscriber::fmt().with_env_filter(default_filter()).with_ansi(true).init();
}

/// 服务模式：滚动文件日志，返回 guard 必须保持存活到进程结束。
fn init_file_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    let appender = tracing_appender::rolling::daily(paths::log_dir(), "dualnic-service.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_env_filter(default_filter())
        .with_writer(writer)
        .with_ansi(false)
        .init();
    guard
}

// ──────────────────────────── CLI 小工具 ────────────────────────────

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn print_help() {
    println!(
        "dualnic-service —— 双网卡分流常驻服务（服务壳 + IPC）\n\
         \n\
         用法：\n\
         \x20 dualnic-service                    以 LocalSystem Windows 服务运行（SCM 启动；仅 Windows）\n\
         \x20 dualnic-service --console [--listen ADDR]   前台调试模式（默认 {DEFAULT_LISTEN_ADDR}）\n\
         \x20 dualnic-service --help              本帮助\n\
         \n\
         环境变量：\n\
         \x20 DUALNIC_CONFIG_PATH     覆盖配置文件路径（默认 %ProgramData%\\DualNIC Balance\\config.json）\n\
         \n\
         说明：服务包含路由对账/自愈与事件日志；写路由需管理员权限。\n\
         GUI 连接：dualnic-gui（窗口）或 dualnic-gui --headless（一次性打印状态）"
    );
}
