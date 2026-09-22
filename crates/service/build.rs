//! 构建期把 assets/icon.ico 嵌入 exe（资源管理器/任务栏显示的文件图标）。
fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        // build script 的工作目录 = 本 crate 根（crates/service）
        winresource::WindowsResource::new()
            .set_icon("../../assets/icon.ico")
            .compile()
            .expect("嵌入应用图标失败（assets/icon.ico）");
    }
}
