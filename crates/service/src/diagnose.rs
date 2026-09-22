//! 一键诊断：执行系统命令（route print / netsh 三件套）+ 意图 vs 实际 diff，组装成报告。
//!
//! 只读：不写路由、不改系统。命令输出按 GBK 解码——中文 Windows 的 cmd 默认代码页是
//! GBK（936），直接按 UTF-8 会乱码，故用 `encoding_rs::GBK` 转成 UTF-8 再交给 GUI。

use std::process::Command;

use dualnic_core::ipc::DiagnoseData;

use crate::status::{config_snapshot, unix_now_secs, RuntimeState};

/// 生成诊断报告。
pub fn diagnose(st: &RuntimeState) -> DiagnoseData {
    let report = {
        let cfg = config_snapshot(st).config;
        match (crate::net::adapters_views(), crate::net::all_ipv4_routes()) {
            (Ok(a), Ok(r)) => dualnic_core::diff::diff_snapshot(&cfg, &a, &r),
            _ => Default::default(),
        }
    };

    let (route_print, mut err): (Vec<String>, Option<dualnic_core::msg::MessageRef>) =
        run_cmd("route", &["print", "-4"]);
    let (net_interfaces, e2) = run_cmd("netsh", &["interface", "ipv4", "show", "interfaces"]);
    let (net_config, e3) = run_cmd("netsh", &["interface", "ipv4", "show", "config"]);
    if err.is_none() {
        err = e2;
    }
    if err.is_none() {
        err = e3;
    }

    DiagnoseData {
        fetched_at_unix_secs: unix_now_secs(),
        report,
        route_print,
        net_interfaces,
        net_config,
        command_error: err,
    }
}

/// 执行命令，返回按行拆分的 stdout（自适应解码，见 [`decode_output`]）。失败返回说明。
fn run_cmd(program: &str, args: &[&str]) -> (Vec<String>, Option<dualnic_core::msg::MessageRef>) {
    match Command::new(program).args(args).output() {
        Ok(out) => {
            let text = decode_output(&out.stdout);
            let lines = text.lines().map(str::to_owned).collect();
            if out.status.success() {
                (lines, None)
            } else {
                let err_text = decode_output(&out.stderr);
                (
                    lines,
                    Some(dualnic_core::msgref!(
                        "DNERR", 90; program,
                        &format!("{:?}", out.status.code()), &err_text
                    )),
                )
            }
        }
        Err(e) => (Vec::new(), Some(dualnic_core::msgref!("DNERR", 91; program, &e.to_string()))),
    }
}

/// 命令输出自适应解码。中文 Windows 上重定向后的编码并不统一：
/// - `netsh` 重定向到管道/文件时输出 **UTF-16LE**（带 BOM）——按 GBK 解就是乱码；
/// - 系统开启「Beta: UTF-8」时输出 UTF-8；
/// - 默认（代码页 936）为 GBK/OEM 文本。
/// 按「BOM → NUL 占比启发式 → UTF-8 → GBK」顺序探测。
fn decode_output(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    // 无 BOM 但 NUL 字节密集（UTF-16 文本特征，ASCII 字符高字节为 0）
    if bytes.len() >= 4
        && bytes.len() % 2 == 0
        && bytes.iter().filter(|b| **b == 0).count() > bytes.len() / 4
    {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let (text, _, _) = encoding_rs::GBK.decode(bytes);
    text.into_owned()
}
