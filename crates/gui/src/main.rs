//! `dualnic-gui` —— 桌面客户端（状态窗 + 本机 IPC）。
//!
//! - 默认：egui 窗口，后台每 2s 轮询服务状态并展示。
//! - `--headless`：不建窗口/渲染，单发一次 GetStatus 打印 JSON 后退出（exit 0/1），
//!   供命令行 / CI 验证「服务 ↔ IPC ↔ 客户端」链路。
#![windows_subsystem = "windows"]

mod app;
mod client;
mod i18n;
mod msgs;
mod probe;
mod setup;
mod state;
mod tray;
mod wizard;

use eframe::egui;
use std::process::ExitCode;

#[cfg(windows)]
fn attach_parent_console() {
    // GUI 子系统不自带控制台；从终端跑 --headless 时挂回父控制台，println 才可见。
    // 双击启动时父进程是 explorer → 挂接失败，静默忽略。
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(dw_process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(windows)]
fn enforce_single_instance() -> bool {
    // 命名互斥体（会话内全局）：第二次启动 ERROR_ALREADY_EXISTS → 弹提示退出。
    // 句柄永不关闭——进程存活期间互斥体必须一直被持有。
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(lp_attributes: *const u32, b_initial_owner: i32, lp_name: *const u16) -> isize;
        fn GetLastError() -> u32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    const ERROR_ALREADY_EXISTS: u32 = 183;
    const MB_OK: u32 = 0x0;
    const MB_ICONINFORMATION: u32 = 0x40;

    let name = to_wide("Local\\DualNICBalanceGUI.SingleInstance");
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if h != 0 && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let text = to_wide(&crate::i18n::tr(msgs::TRAY_ALREADY_RUNNING, &[]));
        let caption = to_wide("DualNIC Balance");
        unsafe { MessageBoxW(0, text.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONINFORMATION) };
        return false;
    }
    true
}

fn main() -> ExitCode {
    #[cfg(windows)]
    attach_parent_console();
    // 语言运行时尽早装载：单实例弹窗/托盘/窗口文案都需要它
    crate::i18n::init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--headless") {
        return run_headless();
    }

    // 单实例：仅窗口模式限制（--headless 是脚本/诊断用途，不拦）
    #[cfg(windows)]
    if !enforce_single_instance() {
        return ExitCode::SUCCESS;
    }
    run_window()
}

/// 单发状态查询，打印 JSON 后退出。
fn run_headless() -> ExitCode {
    // 调试输出也走消息目录（与窗口模式一致的语言体验）
    i18n::init();
    match client::get_status() {
        Ok(st) => {
            match serde_json::to_string_pretty(&st) {
                Ok(json) => println!("{json}"),
                Err(e) => eprintln!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 102; &e.to_string()))),
            }
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 103; &msg)));
            ExitCode::FAILURE
        }
    }
}

fn run_window() -> ExitCode {
    // 语言运行时：按持久化选择（gui.ini）/系统语言装载消息目录（先于任何 UI 渲染）
    i18n::init();
    // 系统托盘（返回的 TrayIcon 存活到本函数结束，即整个窗口生命周期内有效）
    let _tray = tray::setup_tray();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([780.0, 540.0])
        .with_title("DualNIC Balance");
    if let Some(icon) = load_icon(include_bytes!("../../../assets/icon-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    match eframe::run_native(
        "DualNIC Balance",
        native_options,
        Box::new(|cc| Ok(Box::new(app::DualNicApp::new(cc)))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 104; &e.to_string())));
            ExitCode::FAILURE
        }
    }
}

/// 解码内嵌 PNG 为窗口图标（egui::IconData 需要原始 RGBA）。
fn load_icon(png: &[u8]) -> Option<egui::IconData> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    Some(egui::IconData { width: w, height: h, rgba: img.into_raw() })
}
