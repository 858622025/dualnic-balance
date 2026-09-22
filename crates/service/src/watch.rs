//! 网络接口变化事件监听 + 定时兜底 → 自动匹配 + 持续对账自愈。
//!
//! 回调跑在系统线程池，**只置一个 AtomicBool**（零重入、零锁）；真正的 detect/自动动作
//! 收敛到本模块的单一 watcher 线程串行执行，且写操作走 `RuntimeState.write_guard` 与 IPC 串行。
//! 防抖 + 按环境指纹幂等：指纹门只挡「自动匹配」（环境没变不切方案）；「对账」无条件执行
//! ——对账幂等（有差异才写），保证 WAN 默认路由被第三方删除/metric 被改时 10 秒内自愈。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{BOOLEAN, HANDLE};
use windows::Win32::NetworkManagement::IpHelper::{
    CancelMibChangeNotify2, NotifyIpInterfaceChange, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE,
};
use windows::Win32::Networking::WinSock::AF_UNSPEC;

use dualnic_core::env::{compute_env_fingerprint, EnvFingerprint};

use crate::server::Shutdown;
use crate::status::RuntimeState;

/// 事件回调：只置唤醒标志。
unsafe extern "system" fn on_if_change(
    ctx: *const core::ffi::c_void,
    _row: *const MIB_IPINTERFACE_ROW,
    _ntype: MIB_NOTIFICATION_TYPE,
) {
    if !ctx.is_null() {
        let flag = &*(ctx as *const AtomicBool);
        flag.store(true, Ordering::Release);
    }
}

/// 启动 watcher 线程（由 console / SCM 在 server 线程之外调用）。
pub fn start(state: Arc<RuntimeState>, shutdown: Shutdown) {
    std::thread::spawn(move || watcher_loop(state, shutdown));
}

fn watcher_loop(state: Arc<RuntimeState>, shutdown: Shutdown) {
    // wake 标志：回调写它；本线程读它。栈上 Box，函数结束前会先注销回调，故安全。
    let wake = Box::new(AtomicBool::new(false));
    let wake_ptr = &*wake as *const AtomicBool as *const core::ffi::c_void;
    let mut handle = HANDLE(std::ptr::null_mut());

    let registered = unsafe {
        NotifyIpInterfaceChange(
            AF_UNSPEC,
            Some(on_if_change),
            Some(wake_ptr),
            BOOLEAN(1), // initial notification = TRUE → 启动即回调一次（做一次启动检测）
            &mut handle,
        )
    };
    let registered_ok = registered.0 == 0;
    if !registered_ok {
        tracing::warn!(code = registered.0, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 70;)));
    }

    let mut last_poll = Instant::now();
    let mut last_fp: Option<EnvFingerprint> = None;

    loop {
        if shutdown.is_requested() {
            if registered_ok {
                let _ = unsafe { CancelMibChangeNotify2(handle) };
            }
            tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 71;)));
            break;
        }

        if wake.swap(false, Ordering::AcqRel) {
            // 事件唤醒：1.5s 防抖（期间持续 drain，吸收插拔抖动）
            debounce_drain(&wake, Duration::from_millis(1500));
            run_detect(&state, &mut last_fp);
            last_poll = Instant::now();
        } else {
            std::thread::sleep(Duration::from_millis(500));
            if last_poll.elapsed() >= Duration::from_secs(10) {
                last_poll = Instant::now();
                run_detect(&state, &mut last_fp);
            }
        }
    }
    // wake 在此 drop；回调已注销，安全。
}

/// 防抖：期间若有新事件则重新计时，直到安静满 quiet 时长。
fn debounce_drain(flag: &AtomicBool, quiet: Duration) {
    let mut last_event = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(100));
        if flag.swap(false, Ordering::AcqRel) {
            last_event = Instant::now();
        }
        if last_event.elapsed() >= quiet {
            break;
        }
    }
}

/// 周期检测：指纹变了 → 自动匹配切方案；**对账无条件执行**（幂等收敛，无差异零写入）。
///
/// 指纹门只挡「自动匹配」（环境没变不切方案，避免横跳），不挡「对账」——
/// 否则第三方删掉 WAN 默认路由/metric 时指纹不变，服务永远不自愈（真机验证过的缺口）。
fn run_detect(state: &Arc<RuntimeState>, last_fp: &mut Option<EnvFingerprint>) {
    let adapters = match crate::net::adapters_views() {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %dualnic_core::msg::t(&e), "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 72;)));
            return;
        }
    };
    let physical = crate::status::get_physical_nics(state);
    let fp = compute_env_fingerprint(&physical, &adapters);
    if last_fp.as_ref() != Some(&fp) {
        *last_fp = Some(fp);
        match crate::status::auto_match(state) {
            Ok(data) => {
                tracing::info!(action = %data.action, profile = ?data.profile, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 73;)))
            }
            Err(e) => tracing::warn!(error = %dualnic_core::msg::t(&e), "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 74;))),
        }
    }
    // 无论指纹是否变化都对账：对账内部先 diff 实际路由 vs 配置意图，有差异才写，
    // 无差异零写入零事件（log_reconcile 只在动作数 > 0 时记事件），10 秒一次无副作用。
    run_reconcile_if_enabled(state);
}

/// 若对账开启，则跑一次自动对账（try_lock 去重）。
fn run_reconcile_if_enabled(state: &Arc<RuntimeState>) {
    run_reconcile_if_enabled_ref(state);
}

/// 对账开启时跑一次自动对账（供保存配置后主动触发）。取 `&RuntimeState`，无 Arc 包装也能调。
pub fn run_reconcile_if_enabled_ref(state: &RuntimeState) {
    if state.paused.load(Ordering::Acquire) {
        return; // 对账暂停：自动对账不写路由
    }
    let cfg = crate::status::config_snapshot(state).config;
    if !cfg.reconciliation.converge_default_route && !cfg.reconciliation.remove_stale_defaults {
        // 当前方案「不管理路由」→ 清理本工具此前添加的前缀路由（切方案后旧路由不应残留）。
        crate::reconcile::cleanup_tool_routes_if_any(state);
        return;
    }
    match crate::reconcile::try_reconcile(state) {
        Some(r) => {
            tracing::info!(
                removed = r.removed_defaults,
                added = r.added_prefixes,
                metrics = r.metrics_fixed,
                "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 75;))
            )
        }
        None => {} // 上一次对账尚未结束，跳过
    }
}
