//! Windows owns startup registration. OpenRevo only opens the Windows settings surface.
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::HWND,
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    },
};

pub(crate) fn open_settings(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("请从 OpenRevo 主窗口打开 Windows 启动设置".into());
    }
    let owner = window.hwnd().map_err(|error| error.to_string())?;
    let result = unsafe {
        ShellExecuteW(
            HWND(owner.0 as _),
            w!("open"),
            w!("ms-settings:startupapps"),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result.0 as usize) <= 32 {
        return Err(format!("Windows 未打开启动设置（{}）", result.0 as usize));
    }
    Ok(())
}
