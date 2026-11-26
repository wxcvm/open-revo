//! An independent, session-only Windows idle-sleep request for background jobs.
use parking_lot::Mutex;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use tauri::{AppHandle, Emitter};
use windows::{
    core::PWSTR,
    Win32::{
        Foundation::HANDLE,
        System::{
            Power::{PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest},
            Threading::{REASON_CONTEXT, REASON_CONTEXT_0, POWER_REQUEST_CONTEXT_SIMPLE_STRING},
        },
    },
};

const REASON: &str = "OpenRevo Background Awake (ARTEX)";
pub(crate) const EVENT: &str = "background-awake-state-changed";
static REQUEST: Mutex<Option<OwnedHandle>> = Mutex::new(None);

fn acquire() -> windows::core::Result<OwnedHandle> {
    let mut reason = REASON.encode_utf16().chain(std::iter::once(0)).collect::<Vec<_>>();
    let context = REASON_CONTEXT {
        Version: 0, // POWER_REQUEST_CONTEXT_VERSION
        Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
        Reason: REASON_CONTEXT_0 { SimpleReasonString: PWSTR(reason.as_mut_ptr()) },
    };
    // The request object copies the reason. The owned handle also releases the
    // request after a crash/exit, and on failure before storing it.
    unsafe {
        let handle = PowerCreateRequest(&context)?;
        let owned = OwnedHandle::from_raw_handle(handle.0);
        PowerSetRequest(handle, PowerRequestSystemRequired)?;
        Ok(owned)
    }
}

fn apply(request: &mut Option<OwnedHandle>, enabled: bool) -> Result<bool, String> {
    if enabled && request.is_none() {
        *request = Some(acquire().map_err(|err| format!("Windows 未接受后台保活请求：{err}"))?);
    } else if !enabled {
        if let Some(handle) = request.take() {
            // Windows may already have cleared the request on explicit sleep.
            // Closing the sole handle also removes any remaining request.
            let _ = unsafe { PowerClearRequest(HANDLE(handle.as_raw_handle()), PowerRequestSystemRequired) };
        }
    }
    Ok(request.is_some())
}

pub(crate) fn active() -> bool {
    REQUEST.lock().is_some()
}

fn update(app: &AppHandle, enabled: Option<bool>) -> Result<bool, String> {
    let result = {
        let mut request = REQUEST.lock();
        let enable = enabled.unwrap_or(!request.is_some());
        apply(&mut request, enable)
    };
    let ui = app.clone();
    let failed = result.is_err();
    let _ = app.run_on_main_thread(move || {
        let active = active();
        super::tray::update_background_awake(active, failed && !active);
        let _ = ui.emit(EVENT, active);
    });
    result
}

pub(crate) fn toggle_from_tray(app: &AppHandle) {
    if let Err(error) = update(app, None) {
        crate::log_warn!("AWAKE", "{error}");
    }
}

pub(crate) fn release() {
    let _ = apply(&mut REQUEST.lock(), false);
}

pub(crate) fn release_for_sleep() {
    if !active() {
        return;
    }
    // Explicit sleep is not an error. The next session must opt in again.
    if let Some(app) = super::webview_orchestrator::WebviewOrchestrator::global().app() {
        let _ = update(&app, Some(false));
    } else {
        release();
    }
}

#[tauri::command]
pub fn get_background_awake() -> bool {
    active()
}

#[tauri::command]
pub fn set_background_awake(window: tauri::WebviewWindow, enable: bool) -> Result<bool, String> {
    use tauri::Manager;
    if window.label() != "main" {
        return Err("请在 OpenRevo 主窗口或托盘设置后台保活".into());
    }
    update(window.app_handle(), Some(enable))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_awake_native_lifecycle_is_idempotent() {
        let mut request = None;
        assert!(!apply(&mut request, false).unwrap());
        assert!(apply(&mut request, true).unwrap());
        let original = request.as_ref().unwrap().as_raw_handle();
        assert!(apply(&mut request, true).unwrap());
        assert_eq!(original, request.as_ref().unwrap().as_raw_handle());
        assert!(!apply(&mut request, false).unwrap());
        assert!(!apply(&mut request, false).unwrap());
    }

    #[test]
    #[ignore = "requires elevated powercfg /requests for kernel readback"]
    fn background_awake_powercfg_readback() {
        use std::process::Command;
        fn requests() -> String {
            let output = Command::new("powercfg.exe").arg("/requests").output().unwrap();
            assert!(output.status.success(), "{:?}", output.status);
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        let mut request = None;
        assert!(!requests().contains(REASON), "existing OpenRevo request; disable before testing");
        apply(&mut request, true).unwrap();
        let output = requests();
        println!("{output}");
        assert_eq!(output.matches(REASON).count(), 1);
        apply(&mut request, false).unwrap();
        assert!(!requests().contains(REASON), "request must disappear after release");
    }
}
