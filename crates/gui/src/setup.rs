//! 安装辅助：「设置」页的后台动作。
//!
//! - 4 个 PowerShell 脚本**嵌入 exe**（include_str!），运行时写到 `%TEMP%` 再调起——
//!   分发后不依赖 scripts 目录。
//! - 服务注册/卸载需要管理员：ShellExecuteW `runas` 弹一次 UAC（脚本窗口可见，便于看报错）。
//! - 自启动计划任务注册/取消：当前用户普通权限，不弹 UAC。
//! - 状态探测：`sc.exe query`（服务）+ `schtasks /query`（任务），普通权限即可。
//!   所有子进程都带 CREATE_NO_WINDOW，避免 GUI 弹黑色控制台。

use std::os::windows::process::CommandExt;

use crate::msgs;
use std::path::PathBuf;
use std::process::Command;

/// 服务名 / 计划任务名（与 scripts/*.ps1 保持一致）。
pub const SERVICE_NAME: &str = "DualNICBalance";
pub const AUTOSTART_TASK: &str = "DualNICBalanceGUI";

/// 嵌入脚本（源文件 UTF-8 with BOM；PS 5.1 无 BOM 会按 ANSI 解析中文注释）。
pub const INSTALL_SERVICE: &str = include_str!("../../../scripts/install-service.ps1");
pub const UNINSTALL_SERVICE: &str = include_str!("../../../scripts/uninstall-service.ps1");
pub const INSTALL_AUTOSTART: &str = include_str!("../../../scripts/install-gui-autostart.ps1");
pub const UNINSTALL_AUTOSTART: &str = include_str!("../../../scripts/uninstall-gui-autostart.ps1");

/// 隐藏子进程控制台窗口。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 服务安装/运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    NotInstalled,
    Running,
    Stopped,
}

impl ServiceStatus {
    pub fn label(self) -> String {
        match self {
            ServiceStatus::NotInstalled => crate::i18n::tr(msgs::SET_SVC_LBL_NOT_INSTALLED, &[]),
            ServiceStatus::Running => crate::i18n::tr(msgs::SET_SVC_LBL_RUNNING, &[]),
            ServiceStatus::Stopped => crate::i18n::tr(msgs::SET_SVC_LBL_STOPPED, &[]),
        }
    }
}

// ──────────────────────────── 脚本落盘 ────────────────────────────

/// 安装动作的临时工作目录（脚本落盘 + Transcript 运行日志）。
pub fn setup_dir() -> PathBuf {
    std::env::temp_dir().join("dualnic-balance-setup")
}

/// 把嵌入脚本写到 `%TEMP%\dualnic-balance-setup\`（幂等，内容没变不重写）。
pub fn write_script(file_name: &str, content: &str) -> Result<PathBuf, String> {
    let dir = setup_dir();
    std::fs::create_dir_all(&dir).map_err(|e| crate::i18n::tr(msgs::SET_TEMP_DIR_FAIL, &[&e]))?;
    let path = dir.join(file_name);
    // PS 5.1 必须有 UTF-8 BOM 才按 UTF-8 解析；嵌入内容若带 BOM（U+FEFF）则原样保留。
    let mut bytes = Vec::with_capacity(content.len() + 3);
    if !content.starts_with('\u{feff}') {
        bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    bytes.extend_from_slice(content.as_bytes());
    if std::fs::read(&path).map(|old| old != bytes).unwrap_or(true) {
        std::fs::write(&path, bytes).map_err(|e| crate::i18n::tr(msgs::SET_WRITE_SCRIPT_FAIL, &[&e]))?;
    }
    Ok(path)
}

/// 与 GUI 同目录的服务 exe（部署布局：dualnic-service.exe 与 dualnic-gui.exe 同目录）。
pub fn sibling_service_exe() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let p = dir.join("dualnic-service.exe");
    p.is_file().then_some(p)
}

/// 读脚本 Transcript 日志的末尾若干行（失败时展示到 GUI，替代「请看一闪而过的弹窗」）。
/// PS 5.1 的 Start-Transcript 写 UTF-16LE，需按 BOM 解码；顺带滤掉 Transcript 框架行与空行。
pub fn read_log_tail(path: &PathBuf, max_lines: usize) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let kept: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.chars().all(|c| c == '*')
                && !l.contains("Windows PowerShell 脚本")
                && !l.contains("Windows PowerShell transcript")
                && !l.starts_with("PS>")
        })
        .collect();
    let start = kept.len().saturating_sub(max_lines);
    kept[start..].join("；")
}

// ──────────────────────────── 状态探测 ────────────────────────────

/// 查询服务状态（普通权限可查；查询失败一律按未安装处理）。
pub fn service_status() -> ServiceStatus {
    let Ok(out) = Command::new("sc.exe")
        .args(["query", SERVICE_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return ServiceStatus::NotInstalled;
    };
    if !out.status.success() {
        return ServiceStatus::NotInstalled;
    }
    // 输出可能是 GBK，但 STATE 词是 ASCII，直接在字节上找。
    let s = String::from_utf8_lossy(&out.stdout);
    if s.contains("RUNNING") {
        ServiceStatus::Running
    } else {
        ServiceStatus::Stopped
    }
}

/// 登录自启动计划任务是否已注册。
pub fn autostart_registered() -> bool {
    Command::new("schtasks.exe")
        .args(["/query", "/tn", AUTOSTART_TASK])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ──────────────────────────── 执行 ────────────────────────────

/// 普通权限跑脚本（隐藏窗口、捕获输出）。输出可能是 GBK，仅用于失败提示（主要状态靠回查）。
pub fn run_script(script: &PathBuf, args: &[&str]) -> Result<String, String> {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| crate::i18n::tr(msgs::SET_PS_START_FAIL, &[&e]))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let tail: String = text.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("；");
    if out.status.success() {
        Ok(tail)
    } else {
        Err(if tail.is_empty() {
            let code_str = out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into());
            crate::i18n::tr(msgs::SET_SCRIPT_EXIT_CODE, &[&code_str])
        } else { tail })
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 弹 UAC 以管理员运行任意程序（ShellExecuteW runas）。Ok = 已拉起；Err = UAC 取消/被拦截。
fn shell_runas(program: &str, params: &str, show: i32) -> Result<(), String> {
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut core::ffi::c_void,
            verb: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }
    let verb = to_wide("runas");
    let file = to_wide(program);
    let params = to_wide(params);
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            show,
        )
    };
    // >32 = 成功拉起；SE_ERR_ACCESSDENIED(5) 等表示 UAC 被取消/拒绝
    if r > 32 {
        Ok(())
    } else {
        Err(crate::i18n::tr(msgs::SET_UAC_DENIED, &[]))
    }
}

/// 弹 UAC 以管理员跑脚本并**等待其退出**，返回脚本退出码（成败从脚本自身获取，
/// 不再靠 GUI 事后立即回查——那会与「sc stop/delete 需要几秒」竞态误报失败）。
/// 脚本窗口保持可见，便于看报错；超时不杀进程（弹窗留给用户查看），返回 Err。
/// Err = UAC 取消/被拦截、启动失败或等待超时。
pub fn run_script_elevated_wait(script: &PathBuf, args: &[&str], timeout_secs: u32) -> Result<i32, String> {
    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
    const SEE_MASK_NO_ASYNC: u32 = 0x0000_0100;
    const ERROR_CANCELLED: u32 = 1223;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;

    #[repr(C)]
    struct ShellExecuteInfoW {
        cb_size: u32,
        f_mask: u32,
        hwnd: isize,
        verb: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
        inst_app: isize,
        id_list: *mut core::ffi::c_void,
        class: *const u16,
        key_class: isize,
        hot_key: u32,
        icon_or_monitor: isize,
        process: isize,
    }
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn WaitForSingleObject(handle: isize, ms: u32) -> u32;
        fn GetExitCodeProcess(handle: isize, code: *mut u32) -> i32;
        fn CloseHandle(handle: isize) -> i32;
        fn GetLastError() -> u32;
    }

    let mut p = String::from("-NoProfile -ExecutionPolicy Bypass -File \"");
    p.push_str(&script.display().to_string());
    p.push('"');
    for a in args {
        p.push_str(" \"");
        p.push_str(a);
        p.push('"');
    }
    let verb = to_wide("runas");
    let file = to_wide("powershell.exe");
    let params = to_wide(&p);
    let mut info = ShellExecuteInfoW {
        cb_size: std::mem::size_of::<ShellExecuteInfoW>() as u32,
        f_mask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_ASYNC,
        hwnd: 0,
        verb: verb.as_ptr(),
        file: file.as_ptr(),
        params: params.as_ptr(),
        dir: std::ptr::null(),
        show: 1, // SW_SHOWNORMAL：脚本窗口可见，便于看报错
        inst_app: 0,
        id_list: std::ptr::null_mut(),
        class: std::ptr::null(),
        key_class: 0,
        hot_key: 0,
        icon_or_monitor: 0,
        process: 0,
    };
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(if err == ERROR_CANCELLED {
            crate::i18n::tr(msgs::SET_UAC_DENIED, &[])
        } else {
            crate::i18n::tr(msgs::SET_ELEVATE_FAIL, &[&err])
        });
    }
    if info.process == 0 {
        return Err(crate::i18n::tr(msgs::SET_ELEVATE_NO_HANDLE, &[]));
    }
    let ms = timeout_secs.saturating_mul(1000);
    let waited = unsafe { WaitForSingleObject(info.process, ms) };
    if waited == WAIT_TIMEOUT {
        // 不杀进程：窗口留给用户看输出
        return Err(crate::i18n::tr(msgs::SET_SCRIPT_TIMEOUT, &[&timeout_secs]));
    }
    let mut code: u32 = !0;
    unsafe { GetExitCodeProcess(info.process, &mut code) };
    unsafe { CloseHandle(info.process) };
    Ok(code as i32)
}

/// 弹 UAC 以管理员启动 `dualnic-service.exe --console`（隐藏窗口的后台引擎，本次开机有效）。
/// 用于服务未安装时的「首启即用」；路由写入需要管理员，因此必须提权。
pub fn spawn_console_service_elevated() -> Result<(), String> {
    let exe = sibling_service_exe()
        .ok_or_else(|| crate::i18n::tr(msgs::SET_SVC_EXE_MISSING, &[]))?;
    shell_runas(&exe.display().to_string(), "--console", 0 /* SW_HIDE */)
        .map_err(|e| crate::i18n::tr(msgs::SET_ENGINE_START_FAIL, &[&e]))
}

// ──────────────────────────── GUI 自重启 ────────────────────────────

/// 延迟拉起一个新的 GUI 实例（供「装完服务后重启」用：先退出旧实例释放单实例互斥体）。
pub fn restart_gui_delayed() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| crate::i18n::tr(msgs::SET_EXE_PATH_FAIL, &[&e]))?;
    let script = format!("Start-Sleep -Seconds 2; Start-Process -FilePath '{}'", exe.display());
    Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| crate::i18n::tr(msgs::SET_RESTART_SCHED_FAIL, &[&e]))?;
    Ok(())
}

// ──────────────────────────── 首启自动拉起引擎的 optout ────────────────────────────

/// optout 标记目录/文件（用户取消过 UAC 后不再自动弹）。
fn optout_path() -> Option<PathBuf> {
    let dir = std::env::var_os("LOCALAPPDATA")?;
    let mut p = PathBuf::from(dir);
    p.push("DualNICBalance");
    p.push("skip-console-autostart");
    Some(p)
}

/// 用户是否取消过「自动启动后台引擎」。
pub fn console_autostart_opted_out() -> bool {
    optout_path().map(|p| p.is_file()).unwrap_or(false)
}

/// 记录/清除 optout 标记。
pub fn set_console_autostart_optout(opt_out: bool) {
    let Some(p) = optout_path() else { return };
    if opt_out {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, b"");
    } else {
        let _ = std::fs::remove_file(&p);
    }
}
