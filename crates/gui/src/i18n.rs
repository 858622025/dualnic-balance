//! GUI 侧语言运行时：选择持久化（`gui.ini`）+ Windows 界面语言探测 + core 消息运行时装载。
//!
//! - 选择值：`system`（跟随 Windows，缺省）或语言代码（SC / TC / en / ja，见 lang/languages.ini）；
//! - 持久化：`%LOCALAPPDATA%\DualNICBalance\gui.ini`（与 skip-console-autostart 同目录）；
//! - 目录装载：`core::msg::install(code, lang_dir)`，外部 `lang/` 优先、内嵌兜底。

use std::path::PathBuf;

/// `gui.ini` 落点。
fn gui_ini_path() -> Option<PathBuf> {
    let dir = std::env::var_os("LOCALAPPDATA")?;
    let mut p = PathBuf::from(dir);
    p.push("DualNICBalance");
    p.push("gui.ini");
    Some(p)
}

/// 当前语言选择（持久化值；未设置过 = "system"）。
pub fn current_choice() -> String {
    read_choice().unwrap_or_else(|| "system".to_string())
}

fn read_choice() -> Option<String> {
    let text = std::fs::read_to_string(gui_ini_path()?).ok()?;
    let mut in_ui = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_ui = l == "[ui]";
            continue;
        }
        if in_ui {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim() == "language" {
                    let v = v.trim();
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

fn write_choice(choice: &str) -> Result<(), String> {
    let path = gui_ini_path().ok_or_else(|| dualnic_core::msg::t(&dualnic_core::msgref!("DNERR", 141;)))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
        .map_err(|e| dualnic_core::msg::t(&dualnic_core::msgref!("DNERR", 142; &e.to_string())))?;
    }
    std::fs::write(&path, format!("[ui]\nlanguage = {choice}\n"))
        .map_err(|e| dualnic_core::msg::t(&dualnic_core::msgref!("DNERR", 143; &e.to_string())))
}

/// 探测 Windows 界面语言并映射到语言代码；不在预设（SC/TC/en/ja）内 → None。
#[cfg(windows)]
fn detect_system_language() -> Option<String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
        fn LCIDToLocaleName(locale: u32, name: *mut u16, cch: i32, flags: u32) -> i32;
    }
    let langid = unsafe { GetUserDefaultUILanguage() } as u32;
    let mut buf = [0u16; 85];
    let n = unsafe { LCIDToLocaleName(langid, buf.as_mut_ptr(), buf.len() as i32, 0) };
    if n <= 1 {
        return None;
    }
    let name = String::from_utf16_lossy(&buf[..(n - 1) as usize]).to_ascii_lowercase();
    if name.starts_with("zh") {
        if name.contains("tw") || name.contains("hk") || name.contains("mo") || name.contains("hant") {
            Some("TC".to_string())
        } else {
            Some("SC".to_string())
        }
    } else if name.starts_with("en") {
        Some("en".to_string())
    } else if name.starts_with("ja") {
        Some("ja".to_string())
    } else {
        None
    }
}

#[cfg(not(windows))]
fn detect_system_language() -> Option<String> {
    None
}

/// 由「选择」解析实际语言代码：system → 探测；探测失败/空 → SC（产品默认简体中文）。
fn resolve_code(choice: &str) -> String {
    if choice == "system" || choice.is_empty() {
        detect_system_language().unwrap_or_else(|| "SC".to_string())
    } else {
        choice.to_string()
    }
}

/// 语言目录：exe 同目录 `lang/`（分发布局两 exe 同目录）。
fn lang_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(|p| p.join("lang"))
}

/// 启动时装载：按 languages.ini 动态预载全部语言，并应用持久化的选择。
/// 语言文件落后于程序版本时在调试控制台打告警（渲染缺失项自动回退兜底语言，不影响使用）。
pub fn init() {
    let choice = resolve_code(&current_choice());
    for w in dualnic_core::msg::install(lang_dir().as_deref(), &choice) {
        eprintln!("[i18n] {w}");
    }
}

/// 设置页切换：持久化 + 热切换（下一帧生效）。`choice` = "system" 或语言代码。
pub fn set_choice(choice: &str) -> Result<(), String> {
    write_choice(choice)?;
    let code = resolve_code(choice);
    dualnic_core::msg::set_language(&code);
    Ok(())
}

/// 可用语言（core 运行时按 languages.ini 预载的结果，含登记顺序）。
pub fn available() -> Vec<(String, String)> {
    dualnic_core::msg::languages()
        .into_iter()
        .map(|e| (e.code, e.name))
        .collect()
}

/// 「跟随系统」当前探测到的语言代码（设置页展示用）。
pub fn detected_system_language() -> String {
    detect_system_language().unwrap_or_else(|| "—".to_string())
}

/// 渲染一条消息（GUI 抽词统一入口）：`args` 与模板 `&1..&n` 一一对应。
/// 常量见 `crate::msgs`；词条见 `lang/SC.ini` + `lang/en.ini`。
pub fn tr(m: crate::msgs::Mid, args: &[&dyn std::fmt::Display]) -> String {
    let a: Vec<String> = args.iter().map(|x| x.to_string()).collect();
    dualnic_core::msg::t(&dualnic_core::msg::MessageRef {
        msgid: m.0.into(),
        msgno: m.1,
        args: a,
    })
}
