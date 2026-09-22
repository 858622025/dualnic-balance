//! LocalSystem Windows 服务（SCM 启动）：ServiceMain + 状态迁移 + 优雅停止。
//!
//! 生命周期：register 控制回调 → START_PENDING → 建运行时 + bind + spawn server 线程
//! → RUNNING → 阻塞等 Stop → STOP_PENDING → server 线程退出 → STOPPED。
//! Stop 信号与 `--console` 的 Ctrl-C 走同一个 `Shutdown` 令牌（见 `crate::server`）。

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::{define_windows_service, service_dispatcher};

use crate::auth::SessionAuth;
use crate::paths;
use crate::server::{IpcServer, Shutdown};
use crate::status::{config_snapshot, load_runtime, unix_now_secs};

/// SCM 注册的服务名（与 scripts/install-service.ps1 保持一致）。
pub const SERVICE_NAME: &str = "DualNICBalance";

define_windows_service!(ffi_service_main, service_main);

/// 供 main 调用的入口：把生成的 FFI 入口交给系统调度器。**必须**在主线程调用。
/// 非 SCM 上下文（桌面双击运行 exe）会返回错误（典型 1063）。
pub fn start_dispatcher() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

/// 由宏生成的 FFI service_main 委托过来；参数由 SCM 传入（本服务无需）。
fn service_main(_arguments: Vec<OsString>) {
    if let Err(e) = run_service() {
        tracing::error!(error = %e, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 13;)));
        std::process::exit(1);
    }
}

fn set_status(
    handle: &ServiceStatusHandle,
    state: ServiceState,
    controls_accepted: ServiceControlAccept,
    wait_hint: Duration,
) -> windows_service::Result<()> {
    handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint,
        process_id: None,
    })
}

fn run_service() -> Result<(), Box<dyn std::error::Error>> {
    let shutdown = Shutdown::new();
    let stop_shutdown = shutdown.clone();
    let event_handler = move |control: ServiceControl| -> ServiceControlHandlerResult {
        match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 14;)));
                stop_shutdown.request();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    // 注册控制回调 → ServiceStatusHandle，全程用它汇报状态。
    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;
    let _ = set_status(
        &status_handle,
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        Duration::from_secs(30),
    );

    // 启动阶段：目录兜底 + 配置加载 + bind（失败则停在 StartPending，由 SCM 判定失败）。
    if let Err(e) = paths::ensure_dirs() {
        tracing::warn!(error = %e, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 15;)));
    }
    let start_secs = unix_now_secs();
    let (server, addr) = IpcServer::bind(dualnic_core::ipc::DEFAULT_LISTEN_ADDR)?;
    tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 16; &addr.to_string())));

    let state = Arc::new(load_runtime(addr, start_secs));
    let loaded = config_snapshot(&state).loaded;
    tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 17; &loaded.to_string(), &state.config_path.display().to_string())));

    // 服务逻辑跑在独立线程；serve_loop 阻塞直到关闭令牌置位。
    let auth = Arc::new(SessionAuth::new());
    let serve_shutdown = shutdown.clone();
    let serve_state = Arc::clone(&state);
    let serve_auth = Arc::clone(&auth);
    let server_thread = std::thread::spawn(move || server.serve_loop(&serve_state, &serve_auth, &serve_shutdown));

    // 环境自动匹配监听（网络事件 + 定时兜底），与 IPC 服务并行；共用同一 Shutdown 停止。
    crate::watch::start(Arc::clone(&state), shutdown.clone());

    let _ = set_status(
        &status_handle,
        ServiceState::Running,
        ServiceControlAccept::STOP,
        Duration::ZERO,
    );
    tracing::info!("service running (pid={})", std::process::id());

    // 阻塞到 Stop。
    shutdown.wait_stopped();

    let _ = set_status(
        &status_handle,
        ServiceState::StopPending,
        ServiceControlAccept::empty(),
        Duration::from_secs(5),
    );
    // server 线程收到关闭令牌后 ≤~100ms 退出。
    let _ = server_thread.join();
    let _ = set_status(
        &status_handle,
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        Duration::ZERO,
    );
    tracing::info!("service stopped cleanly");
    Ok(())
}
