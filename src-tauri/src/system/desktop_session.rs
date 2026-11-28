//! Standard-user Windows management runtime. Its builder never registers hardware,
//! extension, updater, or task-registration commands.
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, RunEvent,
};
use windows::Win32::{
    Foundation::HANDLE,
    Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    Management,
    Hardware,
    RefuseBackgroundElevation,
    RefuseUnverifiedToken,
}

fn decide(elevated: Option<bool>, background: bool) -> Mode {
    match (elevated, background) {
        (Some(false), _) => Mode::Management,
        (Some(true), false) => Mode::Hardware,
        (Some(true), true) => Mode::RefuseBackgroundElevation,
        (None, _) => Mode::RefuseUnverifiedToken,
    }
}

fn is_background_arg(arg: &std::ffi::OsStr) -> bool {
    arg.eq_ignore_ascii_case("--minimized")
}

fn elevated() -> windows::core::Result<bool> {
    let mut token = HANDLE::default();
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0;
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let _token = OwnedHandle::from_raw_handle(token.0);
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )?;
    }
    Ok(elevation.TokenIsElevated != 0)
}

pub(crate) fn entry() -> (Mode, bool) {
    let background = std::env::args_os()
        .skip(1)
        .any(|arg| is_background_arg(&arg));
    (decide(elevated().ok(), background), background)
}

#[derive(serde::Serialize)]
pub(crate) struct Session {
    pub hardware: bool,
}

pub(crate) fn request_manager(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = super::manager::show(&app).await {
            crate::log_warn!("MANAGER", "Could not open management center: {error}");
        }
    });
}

fn setup_tray(app: &AppHandle) -> tauri::Result<super::tray::TrayKeeper> {
    let open = MenuItem::with_id(app, "manager-open", "打开管理中心", true, None::<&str>)?;
    let exit = MenuItem::with_id(app, "manager-exit", "退出 OpenRevo", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &exit])?;
    let mut builder = TrayIconBuilder::with_id("openrevo-management")
        .tooltip("OpenRevo · 标准权限管理中心")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "manager-open" => request_manager(app),
            "manager-exit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                request_manager(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    Ok(super::tray::TrayKeeper(builder.build(app)?))
}

pub(crate) fn run(background: bool) {
    use super::{background_awake, manager, resources, tray::TrayKeeper};
    use crate::commands::sysinfo_cmd;

    crate::system::logger::install_panic_hook();
    let builder = tauri::Builder::default()
        .manage(Session { hardware: false })
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args
                .iter()
                .skip(1)
                .any(|arg| is_background_arg(arg.as_ref()))
            {
                request_manager(app);
            }
        }))
        .invoke_handler(tauri::generate_handler![
            manager::manager_session,
            manager::manager_health,
            manager::manager_inventory,
            manager::manager_startup,
            manager::manager_shell,
            manager::manager_scan,
            manager::manager_scan_status,
            manager::manager_scan_page,
            manager::manager_scan_cancel,
            manager::manager_reveal,
            manager::manager_preview,
            manager::manager_apply,
            manager::manager_receipts,
            manager::manager_peaks,
            manager::manager_save_volume_snapshot,
            sysinfo_cmd::get_resource_snapshot,
            sysinfo_cmd::release_resource_view,
            sysinfo_cmd::get_resource_history,
            sysinfo_cmd::set_resource_history,
            sysinfo_cmd::clear_resource_history,
            sysinfo_cmd::export_resource_history_csv,
            background_awake::get_background_awake_status,
            background_awake::set_background_awake,
        ])
        .setup(|app| {
            if let Ok(dir) = app.path().app_local_data_dir() {
                resources::start(dir.join("resource-history-v1"));
            }
            super::power_listener::start_power_event_watcher();
            // A missing system tray must not turn an explicit launch into a hidden no-op.
            match setup_tray(app.handle()) {
                Ok(tray) => {
                    app.manage(tray);
                }
                Err(error) => {
                    crate::log_warn!("MANAGER", "Management tray unavailable: {error}");
                }
            }
            Ok(())
        });

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(error) => {
            crate::log_error!(
                "MANAGER",
                "Management runtime initialization failed: {error}"
            );
            return;
        }
    };
    app.run(move |app, event| match event {
        RunEvent::Ready if !background || app.try_state::<TrayKeeper>().is_none() => {
            request_manager(app);
        }
        RunEvent::ExitRequested {
            code: None, api, ..
        } if app.try_state::<TrayKeeper>().is_some() => {
            api.prevent_exit();
        }
        RunEvent::Exit => {
            background_awake::release();
            resources::flush_on_exit();
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_an_explicit_elevated_foreground_launch_can_open_hardware_mode() {
        assert_eq!(decide(Some(false), false), Mode::Management);
        assert_eq!(decide(Some(false), true), Mode::Management);
        assert_eq!(decide(None, false), Mode::RefuseUnverifiedToken);
        assert_eq!(decide(None, true), Mode::RefuseUnverifiedToken);
        assert_eq!(decide(Some(true), true), Mode::RefuseBackgroundElevation);
        assert_eq!(decide(Some(true), false), Mode::Hardware);
    }
}
