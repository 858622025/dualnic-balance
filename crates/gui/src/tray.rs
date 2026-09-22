//! 系统托盘：托盘图标 + 右键菜单（显示窗口 / 暂停恢复分流 / 退出）。
//!
//! 托盘菜单事件在独立线程处理；对窗口的控制走 egui 的 `ViewportCommand`，
//! 对服务的控制走本机 IPC（与窗口内按钮同一套 client 方法）。

use eframe::egui;

use crate::msgs;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// 全局 egui Context（app::DualNicApp::new 里设置；托盘事件用它发窗口命令）。
static EGUI_CTX: std::sync::Mutex<Option<egui::Context>> = std::sync::Mutex::new(None);
/// 「用户真的想退出」标志：托盘菜单点「退出」时置位；窗口点 X 时只有它为 true 才放行关闭，
/// 否则只隐藏到托盘。防止 X 直接杀死 GUI。
static QUIT_WANTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 由 app 启动时登记 egui Context，供托盘菜单控制窗口。
pub fn set_egui_ctx(ctx: egui::Context) {
    *EGUI_CTX.lock().unwrap() = Some(ctx);
}

fn egui_ctx() -> Option<egui::Context> {
    EGUI_CTX.lock().unwrap().clone()
}

/// 是否「用户确认退出」（托盘菜单点退出 → true；窗口点 X 应保持 false，只隐藏）。
pub fn quit_wanted() -> bool {
    QUIT_WANTED.load(std::sync::atomic::Ordering::Acquire)
}

/// 让窗口关闭请求「生效」：置位标志并**直接结束进程**。
/// 这里不依赖 egui 的 close 事件协商（隐藏到托盘时窗口不跑帧，收不到 close，会卡死），
/// 直接 `std::process::exit(0)` 最可靠 —— GUI 是纯显示客户端，无强制清理状态。
pub fn request_quit() {
    QUIT_WANTED.store(true, std::sync::atomic::Ordering::Release);
    std::process::exit(0);
}

/// 创建托盘图标并启动菜单事件线程。返回的 TrayIcon 必须保持存活到进程结束。
pub fn setup_tray() -> Option<TrayIcon> {
    let menu = Menu::new();
    let show_item = MenuItem::new(&crate::i18n::tr(msgs::TRAY_SHOW, &[]), true, None);
    let pause_item = MenuItem::new(&crate::i18n::tr(msgs::TRAY_PAUSE_RESUME, &[]), true, None);
    let quit_item = MenuItem::new(&crate::i18n::tr(msgs::TRAY_QUIT, &[]), true, None);
    menu.append_items(&[&show_item, &pause_item, &quit_item]).ok()?;

    let icon = make_icon()?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("DualNIC Balance")
        .with_icon(icon)
        .build()
        .ok()?;

    let show_id = show_item.id().clone();
    let pause_id = pause_item.id().clone();
    let quit_id = quit_item.id().clone();
    let receiver = MenuEvent::receiver();
    std::thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            if event.id == show_id {
                if let Some(ctx) = egui_ctx() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                }
            } else if event.id == pause_id {
                toggle_paused();
            } else if event.id == quit_id {
                request_quit(); // 置位退出标志后再关闭 → 窗口放行真正退出
            }
        }
    });

    Some(tray)
}

/// 暂停/恢复对账：查当前 paused → 置反。
fn toggle_paused() {
    std::thread::spawn(|| {
        let Ok(st) = crate::client::get_status() else { return };
        let Ok(token) = crate::client::handshake() else { return };
        let _ = crate::client::set_paused(&token, !st.paused);
    });
}

/// 托盘图标：内嵌 assets/icon-32.png（与应用/exe 同一图标源）。
fn make_icon() -> Option<Icon> {
    let png = include_bytes!("../../../assets/icon-32.png");
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    Icon::from_rgba(img.into_raw(), w, h).ok()
}
