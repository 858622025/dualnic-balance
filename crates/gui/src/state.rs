//! GUI 后台状态：轮询线程与 UI 共享态。

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dualnic_core::ipc::{DiagnoseData, EventsData, SnapshotData, StatusData};

use crate::msgs;

pub type PollOutcome = Result<StatusData, String>;

/// UI 侧共享轮询结果快照（每次 IPC 都是新连接 = 天然热重连）。
#[derive(Clone, Default)]
pub struct PollState {
    /// 最近一次轮询结果（None = 尚未完成首次）。
    pub last: Option<PollOutcome>,
    /// 最近一次成功的时间（unix 秒）。
    pub last_ok_unix: u64,
    /// UI「立即刷新」请求位；轮询线程消费后清空。
    pub force: bool,
}

pub fn unix_now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 启动后台轮询线程：`force` 或距上次轮询已过 `interval` 就发一次 GetStatus，
/// 写共享态后 `ctx.request_repaint()` 唤醒 UI 重绘。
pub fn spawn_poll_thread(ctx: eframe::egui::Context, shared: Arc<Mutex<PollState>>, interval: Duration) {
    thread::spawn(move || {
        let now = Instant::now();
        let mut last_poll = now.checked_sub(interval).unwrap_or(now);
        loop {
            thread::sleep(Duration::from_millis(200));

            let forced = {
                let mut s = shared.lock().unwrap();
                let f = s.force;
                s.force = false;
                f
            };
            if !(forced || last_poll.elapsed() >= interval) {
                continue;
            }
            last_poll = Instant::now();

            let outcome = crate::client::get_status();
            {
                let mut s = shared.lock().unwrap();
                if outcome.is_ok() {
                    s.last_ok_unix = unix_now_secs();
                }
                s.last = Some(outcome);
            }
            ctx.request_repaint();
        }
    });
}

// ──────────────────────────── 快照（GetSnapshot，独立单发） ────────────────────────────

/// 快照共享态：只在用户切到快照页 / 点刷新时取一次，不进 2s 轮询。
#[derive(Default)]
pub struct SnapshotState {
    /// 最近一次快照结果。
    pub last: Option<Result<SnapshotData, String>>,
    /// 是否正在拉取（防并发重复）。
    pub in_flight: bool,
}

/// 单发拉取快照（幂等：已在飞则跳过），完成后回写并请求重绘。
pub fn fetch_snapshot_async(ctx: eframe::egui::Context, shared: Arc<Mutex<SnapshotState>>) {
    {
        let mut s = shared.lock().unwrap();
        if s.in_flight || s.last.is_some() {
            return; // 已有数据时用「立即刷新」显式重取，避免每次帧都打
        }
        s.in_flight = true;
    }
    let ctx2 = ctx.clone();
    thread::spawn(move || {
        let outcome = crate::client::get_snapshot();
        {
            let mut s = shared.lock().unwrap();
            s.last = Some(outcome);
            s.in_flight = false;
        }
        ctx2.request_repaint();
    });
}

/// 显式刷新（清掉旧数据强制重取）。
pub fn refresh_snapshot_async(ctx: eframe::egui::Context, shared: Arc<Mutex<SnapshotState>>) {
    {
        let mut s = shared.lock().unwrap();
        if s.in_flight {
            return;
        }
        s.in_flight = true;
        s.last = None;
    }
    let ctx2 = ctx.clone();
    thread::spawn(move || {
        let outcome = crate::client::get_snapshot();
        {
            let mut s = shared.lock().unwrap();
            s.last = Some(outcome);
            s.in_flight = false;
        }
        ctx2.request_repaint();
    });
}

// ──────────────────────────── 诊断（Diagnose，独立单发） ────────────────────────────

/// 诊断共享态：每次进诊断页（自动「立即诊断」一次）/ 点「立即诊断」按钮时取。
#[derive(Default)]
pub struct DiagnoseState {
    pub last: Option<Result<DiagnoseData, String>>,
    pub in_flight: bool,
}

/// 显式（重新）诊断：清掉旧数据强制重取；幂等（已在飞则跳过）。
/// 进诊断页签的自动触发也走本函数（app.rs 的 diag_was_active 模式）。
pub fn refresh_diagnose_async(ctx: eframe::egui::Context, shared: Arc<Mutex<DiagnoseState>>) {
    {
        let mut s = shared.lock().unwrap();
        if s.in_flight {
            return;
        }
        s.in_flight = true;
        s.last = None;
    }
    let ctx2 = ctx.clone();
    thread::spawn(move || {
        let outcome = crate::client::diagnose();
        {
            let mut s = shared.lock().unwrap();
            s.last = Some(outcome);
            s.in_flight = false;
        }
        ctx2.request_repaint();
    });
}

// ──────────────────────────── 事件日志（GetEvents，独立单发） ────────────────────────────

/// 事件日志共享态：进事件页 / 点刷新时取一次。
#[derive(Default)]
pub struct EventsState {
    pub last: Option<Result<EventsData, String>>,
    pub in_flight: bool,
    /// 正在执行「清空日志」。
    pub clearing: bool,
    /// 最近一次清空的结果（成功/失败消息）。
    pub clear_result: Option<Result<(), String>>,
}

/// 单发拉取事件日志（幂等：已在飞则跳过）。
pub fn fetch_events_async(ctx: eframe::egui::Context, shared: Arc<Mutex<EventsState>>) {
    {
        let mut s = shared.lock().unwrap();
        if s.in_flight || s.last.is_some() {
            return;
        }
        s.in_flight = true;
    }
    let ctx2 = ctx.clone();
    thread::spawn(move || {
        let outcome = crate::client::get_events(None, 200);
        {
            let mut s = shared.lock().unwrap();
            s.last = Some(outcome);
            s.in_flight = false;
        }
        ctx2.request_repaint();
    });
}

/// 显式刷新事件日志（清掉旧数据强制重取）。
pub fn refresh_events_async(ctx: eframe::egui::Context, shared: Arc<Mutex<EventsState>>) {
    {
        let mut s = shared.lock().unwrap();
        if s.in_flight {
            return;
        }
        s.in_flight = true;
        s.last = None;
    }
    let ctx2 = ctx.clone();
    thread::spawn(move || {
        let outcome = crate::client::get_events(None, 200);
        {
            let mut s = shared.lock().unwrap();
            s.last = Some(outcome);
            s.in_flight = false;
        }
        ctx2.request_repaint();
    });
}

// ──────────────────────────── 设置页（服务注册 / 登录自启动） ────────────────────────────

/// 设置页共享态：状态探测与安装动作的后台线程写，UI 读。
#[derive(Default)]
pub struct SettingsState {
    pub loaded: bool,
    pub loading: bool,
    /// 服务安装/运行状态（None = 尚未探测）。
    pub svc: Option<crate::setup::ServiceStatus>,
    /// 登录自启动是否已注册（None = 尚未探测）。
    pub autostart: Option<bool>,
    /// 与 GUI 同目录的服务 exe 是否存在（分发布局自检）。
    pub svc_exe_exists: Option<bool>,
    /// 正在执行的动作名（按钮禁用 + 提示）。
    pub busy: Option<String>,
    /// 最近一次动作结果。
    pub outcome: Option<Result<String, String>>,
}

/// 探测服务/自启动当前状态（进设置页或动作完成后调用；动作执行中跳过）。
pub fn load_settings_async(ctx: eframe::egui::Context, shared: Arc<Mutex<SettingsState>>) {
    {
        let mut s = shared.lock().unwrap();
        if s.loading || s.busy.is_some() {
            return;
        }
        s.loading = true;
    }
    thread::spawn(move || {
        let svc = crate::setup::service_status();
        let autostart = crate::setup::autostart_registered();
        let svc_exe_exists = crate::setup::sibling_service_exe().is_some();
        let mut s = shared.lock().unwrap();
        s.svc = Some(svc);
        s.autostart = Some(autostart);
        s.svc_exe_exists = Some(svc_exe_exists);
        s.loaded = true;
        s.loading = false;
        ctx.request_repaint();
    });
}

/// 通用动作骨架：置 busy → 后台执行 → 回查状态 → outcome。
fn run_setup_action<F>(ctx: eframe::egui::Context, shared: Arc<Mutex<SettingsState>>, busy: &str, f: F)
where
    F: FnOnce() -> Result<String, String> + Send + 'static,
{
    {
        let mut s = shared.lock().unwrap();
        if s.busy.is_some() {
            return;
        }
        s.busy = Some(busy.to_string());
        s.outcome = None;
    }
    thread::spawn(move || {
        let r = f();
        // 动作完成后回查真实状态（提权脚本是异步拉起的，多等一拍）
        if r.is_ok() {
            thread::sleep(Duration::from_secs(4));
        }
        let mut s = shared.lock().unwrap();
        if r.is_ok() {
            s.svc = Some(crate::setup::service_status());
            s.autostart = Some(crate::setup::autostart_registered());
        }
        s.busy = None;
        s.outcome = Some(r);
        drop(s);
        ctx.request_repaint();
    });
}

/// 安装 Windows 服务：弹 UAC（脚本会先杀残留引擎进程，再 sc create+start）。
/// 安装成功且服务 Running 时：提示后**自动重启 GUI**——旧实例退出释放 IPC 端口
/// 与单实例互斥体，新实例直接连上系统托管的服务（临时引擎已被脚本清理）。
pub fn install_service_async(ctx: eframe::egui::Context, shared: Arc<Mutex<SettingsState>>) {
    {
        let mut s = shared.lock().unwrap();
        if s.busy.is_some() {
            return;
        }
        s.busy = Some(crate::i18n::tr(msgs::SET_INSTALL_BTN, &[]));
        s.outcome = None;
    }
    thread::spawn(move || {
        let r = (|| -> Result<String, String> {
            let script = crate::setup::write_script("install-service.ps1", crate::setup::INSTALL_SERVICE)?;
            let exe = crate::setup::sibling_service_exe()
                .ok_or_else(|| crate::i18n::tr(msgs::SET_SVC_EXE_MISSING, &[]))?;
            // 等待安装脚本退出（90s），成败优先取脚本退出码；失败时读 Transcript 日志尾部展示真实报错
            let log = crate::setup::setup_dir().join("install-service.log");
            let _ = std::fs::remove_file(&log);
            let code = crate::setup::run_script_elevated_wait(
                &script,
                &["-Exe", &exe.display().to_string(), "-Log", &log.display().to_string()],
                90,
            )?;
            if code != 0 {
                let tail = crate::setup::read_log_tail(&log, 8);
                return Err(if tail.is_empty() {
                    crate::i18n::tr(msgs::SET_INSTALL_FAIL_EXIT, &[&code])
                } else {
                    crate::i18n::tr(msgs::SET_INSTALL_FAIL_TAIL, &[&code, &tail])
                });
            }
            // 服务启动需要几秒（SCM 拉起 + IPC 绑定），轮询等待
            for _ in 0..30 {
                if crate::setup::service_status() == crate::setup::ServiceStatus::Running {
                    return Ok(crate::i18n::tr(msgs::SET_INSTALL_RUNNING_OK, &[]));
                }
                thread::sleep(Duration::from_millis(500));
            }
            match crate::setup::service_status() {
                crate::setup::ServiceStatus::Stopped => {
                    Ok(crate::i18n::tr(msgs::SET_INSTALL_STOPPED_OK, &[]))
                }
                _ => Err(crate::i18n::tr(msgs::SET_INSTALL_NOT_APPLIED, &[])),
            }
        })();
        // 重启判定改为与翻译后文案精确比对（原 contains 中文子串在英文下失配）
        let restart = matches!(&r, Ok(msg) if *msg == crate::i18n::tr(msgs::SET_INSTALL_RUNNING_OK, &[]));
        {
            let mut s = shared.lock().unwrap();
            s.busy = None;
            s.outcome = Some(r);
            if restart {
                s.svc = Some(crate::setup::ServiceStatus::Running);
            } else {
                s.svc = Some(crate::setup::service_status());
                s.autostart = Some(crate::setup::autostart_registered());
            }
        }
        ctx.request_repaint();
        if restart {
            // 让用户看到提示，再退出旧实例（新实例由延迟进程拉起）
            thread::sleep(Duration::from_secs(2));
            let _ = crate::setup::restart_gui_delayed();
            std::process::exit(0);
        }
    });
}

/// 卸载 Windows 服务（弹 UAC）：等待卸载脚本退出，按脚本退出码给结论。
pub fn uninstall_service_async(ctx: eframe::egui::Context, shared: Arc<Mutex<SettingsState>>) {
    run_setup_action(ctx, shared, &crate::i18n::tr(msgs::SET_UNINSTALL_BTN, &[]), || {
        let script = crate::setup::write_script("uninstall-service.ps1", crate::setup::UNINSTALL_SERVICE)?;
        let log = crate::setup::setup_dir().join("uninstall-service.log");
        let _ = std::fs::remove_file(&log);
        let code = crate::setup::run_script_elevated_wait(
            &script,
            &["-Log", &log.display().to_string()],
            60,
        )?;
        let tail = crate::setup::read_log_tail(&log, 8);
        match code {
            // 脚本终判成功（sc query 1060 = 服务已不存在）
            0 => {
                if crate::setup::service_status() == crate::setup::ServiceStatus::NotInstalled {
                    Ok(crate::i18n::tr(msgs::SET_UNINSTALL_OK, &[]))
                } else {
                    Err(crate::i18n::tr(msgs::SET_UNINSTALL_STILL_THERE, &[]))
                }
            }
            // 脚本终判：删除后服务仍在（可能被失败恢复策略拉回）
            2 => Err(if tail.is_empty() {
                crate::i18n::tr(msgs::SET_UNINSTALL_NOT_DONE, &[])
            } else {
                crate::i18n::tr(msgs::SET_UNINSTALL_NOT_DONE_TAIL, &[&tail])
            }),
            n => Err(if tail.is_empty() {
                crate::i18n::tr(msgs::SET_UNINSTALL_FAIL_EXIT, &[&n])
            } else {
                crate::i18n::tr(msgs::SET_UNINSTALL_FAIL_TAIL, &[&n, &tail])
            }),
        }
    });
}

/// 注册登录自启动计划任务：先试普通权限；根目录登录触发任务实际需要管理员，
/// 失败自动回退提权（弹 UAC）。成败以回查任务是否在册为准。
pub fn install_autostart_async(ctx: eframe::egui::Context, shared: Arc<Mutex<SettingsState>>) {
    run_setup_action(ctx, shared, &crate::i18n::tr(msgs::SET_AUTO_REG_BTN, &[]), || {
        let script = crate::setup::write_script("install-gui-autostart.ps1", crate::setup::INSTALL_AUTOSTART)?;
        let exe = std::env::current_exe().map_err(|e| crate::i18n::tr(msgs::SET_EXE_PATH_FAIL, &[&e]))?;
        let arg = exe.display().to_string();
        let _ = crate::setup::run_script(&script, &["-Exe", &arg]);
        if !crate::setup::autostart_registered() {
            // 普通权限失败 → 提权重试：等待脚本退出（计划任务注册需要几秒），
            // 否则立即回查 registered 会与脚本竞态误报失败。
            let code = crate::setup::run_script_elevated_wait(&script, &["-Exe", &arg], 30)?;
            if code != 0 {
                return Err(crate::i18n::tr(msgs::SET_AUTOREG_FAIL_EXIT, &[&code]));
            }
        }
        if crate::setup::autostart_registered() {
            Ok(crate::i18n::tr(msgs::SET_AUTOREG_OK, &[]))
        } else {
            Err(crate::i18n::tr(msgs::SET_AUTOREG_NOT_APPLIED, &[]))
        }
    });
}

/// 取消登录自启动计划任务：同注册，失败自动回退提权。
pub fn uninstall_autostart_async(ctx: eframe::egui::Context, shared: Arc<Mutex<SettingsState>>) {
    run_setup_action(ctx, shared, &crate::i18n::tr(msgs::SET_AUTO_UNREG_BTN, &[]), || {
        let script = crate::setup::write_script("uninstall-gui-autostart.ps1", crate::setup::UNINSTALL_AUTOSTART)?;
        let _ = crate::setup::run_script(&script, &[]);
        if crate::setup::autostart_registered() {
            // 普通权限删不掉（任务可能注册为管理员）→ 提权重试：等脚本退出再回查
            let code = crate::setup::run_script_elevated_wait(&script, &[], 30)?;
            if code != 0 {
                return Err(crate::i18n::tr(msgs::SET_AUTOUNREG_FAIL_EXIT, &[&code]));
            }
        }
        if !crate::setup::autostart_registered() {
            Ok(crate::i18n::tr(msgs::SET_AUTOUNREG_OK, &[]))
        } else {
            Err(crate::i18n::tr(msgs::SET_AUTOUNREG_NOT_APPLIED, &[]))
        }
    });
}

// ──────────────────────────── 首启自动拉起后台引擎 ────────────────────────────

/// GUI 启动时调用一次：服务可用（IPC 通）则不打扰；未安装且 IPC 不通则
/// 弹 UAC 拉起 `dualnic-service.exe --console`（临时引擎，本次开机有效）。
/// 用户取消过 UAC 会记录 optout，之后不再自动弹。结果进横幅（状态页展示）。
pub fn ensure_backend_async(
    ctx: eframe::egui::Context,
) -> Arc<Mutex<Option<Result<String, String>>>> {
    let shared: Arc<Mutex<Option<Result<String, String>>>> = Arc::new(Mutex::new(None));
    let shared2 = shared.clone();
    let ctx2 = ctx.clone();
    thread::spawn(move || {
        let shared = shared2;
        thread::sleep(Duration::from_millis(800)); // 先给轮询线程一次 IPC 机会
        if crate::client::get_status().is_ok() {
            return; // 后台已在跑（服务或手动引擎），不打扰
        }
        match crate::setup::service_status() {
            crate::setup::ServiceStatus::Running => {
                // 服务在跑但 IPC 未就绪：再等一拍，仍不通才提示
                thread::sleep(Duration::from_secs(3));
                if crate::client::get_status().is_ok() {
                    return;
                }
                *shared.lock().unwrap() = Some(Err(
                    crate::i18n::tr(msgs::SET_BANNER_SVC_IPC_WAIT, &[]),
                ));
            }
            crate::setup::ServiceStatus::Stopped => {
                *shared.lock().unwrap() = Some(Err(
                    crate::i18n::tr(msgs::SET_BANNER_SVC_STOPPED, &[]),
                ));
            }
            crate::setup::ServiceStatus::NotInstalled => {
                if crate::setup::console_autostart_opted_out() {
                    *shared.lock().unwrap() = Some(Err(
                        crate::i18n::tr(msgs::SET_BANNER_OPTOUT, &[]),
                    ));
                } else {
                    match crate::setup::spawn_console_service_elevated() {
                        Ok(()) => {
                            for _ in 0..10 {
                                thread::sleep(Duration::from_millis(500));
                                if crate::client::get_status().is_ok() {
                                    *shared.lock().unwrap() = Some(Ok(
                                        crate::i18n::tr(msgs::SET_BANNER_TEMP_ENGINE, &[]),
                                    ));
                                    ctx2.request_repaint();
                                    return;
                                }
                            }
                            *shared.lock().unwrap() = Some(Err(
                                crate::i18n::tr(msgs::SET_BANNER_ENGINE_IPC_FAIL, &[]),
                            ));
                        }
                        Err(msg) => {
                            // 用户取消 UAC：记录 optout，不再每次启动都弹
                            crate::setup::set_console_autostart_optout(true);
                            *shared.lock().unwrap() = Some(Err(
                                crate::i18n::tr(msgs::SET_BANNER_OPTOUT_SAVED, &[&msg]),
                            ));
                        }
                    }
                }
            }
        }
        ctx2.request_repaint();
    });
    shared
}

/// 清空全部事件日志（握手 + ClearEvents），成功后重拉事件刷新库大小。
pub fn clear_events_async(ctx: eframe::egui::Context, shared: Arc<Mutex<EventsState>>) {
    {
        let mut s = shared.lock().unwrap();
        if s.clearing {
            return;
        }
        s.clearing = true;
        s.clear_result = None;
    }
    let ctx2 = ctx.clone();
    thread::spawn(move || {
        let r = crate::client::handshake()
            .map_err(|e| e.message)
            .and_then(|token| crate::client::clear_events(&token).map_err(|e| e.message));
        let ok = r.is_ok();
        {
            let mut s = shared.lock().unwrap();
            s.clearing = false;
            s.clear_result = Some(r.map(|_| ()));
        }
        if ok {
            // 成功才重拉：拿到清空后的真实库大小；失败时保留旧列表便于排查。
            let _ = crate::client::get_events(None, 200).map(|d| {
                let mut s = shared.lock().unwrap();
                s.last = Some(Ok(d));
            });
        }
        ctx2.request_repaint();
    });
}
