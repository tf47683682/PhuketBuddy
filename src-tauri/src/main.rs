#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 关闭 WebView2 的 GPU 合成：部分显卡驱动下透明窗口会被渲染成黑底
    if std::env::var_os("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").is_none() {
        std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--disable-gpu-compositing");
    }
    phuketbuddy_lib::run()
}
