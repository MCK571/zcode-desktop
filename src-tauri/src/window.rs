// 窗口层 — 对齐 Electron 版 main.js（窗口）+ windowsBackdrop.js（Acrylic）
// + windowsChrome.js（圆角/去边框）。
// 真磨砂：tauri 内置 Acrylic 窗口效果（Windows 上走 SetWindowCompositionAttribute
// WCA_ACCENT_POLICY，与 koffi recipe 同源），页面透明像素显示窗口背景模糊。

use tauri::utils::config::{Color, WindowEffectsConfig};
use tauri::utils::WindowEffect;
use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE,
};

pub const WINDOW_W: f64 = 322.0;
pub const WINDOW_H: f64 = 840.0;
pub const WIN_X: f64 = 1260.0;
pub const WIN_Y: f64 = 80.0;
pub const ICON_W: f64 = 48.0;
pub const ICON_H: f64 = 48.0;

// 页面透明底色 → 显示 DWM Acrylic 模糊（0x3a232323 对齐 Electron DEFAULT_ACCENT_ARGB）
const ACCENT_TINT: Color = Color(0x23, 0x23, 0x23, 0x3a);

pub fn create_main(app: &AppHandle) -> tauri::Result<()> {
    let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("ZCode Usage Widget")
        .inner_size(WINDOW_W, WINDOW_H)
        .position(WIN_X, WIN_Y)
        .decorations(false)
        .transparent(true) // WebView2 背景透明 → 页面透明区显示窗口 Acrylic 背景
        .always_on_top(true)
        .resizable(false)
        .shadow(false)
        .background_color(Color(0, 0, 0, 0))
        .build()?;
    apply_chrome(&win);
    Ok(())
}

// 窗口装饰（Acrylic 模糊 + 圆角 + 去 1px 边框）。显示流程 / resize 可能重置
// DWM 属性（Electron 版实测折叠后出现白边），调用方在关键时机重设。
pub fn apply_chrome(win: &WebviewWindow) {
    // 真磨砂：tauri 内置 Acrylic（WCA_ACCENT_POLICY AccentBlurBehind，同 koffi recipe）
    let _ = win.set_effects(WindowEffectsConfig {
        effects: vec![WindowEffect::Acrylic],
        state: None,
        radius: None,
        color: Some(ACCENT_TINT),
    });
    // 系统圆角（抗锯齿）+ 去 1px 系统边框，对齐 windowsChrome.js
    unsafe {
        let hwnd = win.hwnd().unwrap_or_default().0; // windows crate HWND.0 = *mut c_void
        let round: u32 = 2; // DWMWCP_ROUND
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &round as *const u32 as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
        let none = DWMWA_COLOR_NONE as u32;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            &none as *const u32 as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

// 前端全链路用 CSS 像素（对齐 Electron DIP 语义），命令层进出统一换算
pub fn scale_factor(win: &WebviewWindow) -> f64 {
    win.scale_factor().unwrap_or(1.0)
}
