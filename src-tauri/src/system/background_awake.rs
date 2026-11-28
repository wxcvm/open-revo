//! An independent, session-only Windows idle-sleep request for background jobs.
use parking_lot::Mutex;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::time::{Duration, Instant};
use tauri::async_runtime::JoinHandle;
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
pub(crate) const STATUS_EVENT: &str = "background-awake-status-changed";

#[derive(Clone, Copy, serde::Serialize)]
pub struct AwakeStatus {
    pub enabled: bool,
    pub remaining_ms: Option<u64>,
}

struct Lease {
    request: Option<OwnedHandle>,
    deadline: Option<Instant>,
    generation: u64,
    timer: Option<JoinHandle<()>>,
}

impl Lease {
    const fn new() -> Self {
        Self { request: None, deadline: None, generation: 0, timer: None }
    }

    fn set(&mut self, enabled: bool, minutes: Option<u16>, now: Instant) -> Result<(bool, Option<Instant>), String> {
        let deadline = if enabled {
            match minutes {
                Some(value @ 1..=720) => Some(now + Duration::from_secs(u64::from(value) * 60)),
                Some(_) => return Err("定时保活时长须在 1–720 分钟之间".into()),
                None => None,
            }
        } else { None };
        let enabled = apply(&mut self.request, enabled)?;
        if let Some(previous) = self.timer.take() {
            previous.abort();
        }
        self.generation = self.generation.wrapping_add(1);
        self.deadline = deadline;
        Ok((enabled, deadline))
    }

    fn expire(&mut self, generation: u64, now: Instant) -> bool {
        if self.generation != generation || !self.deadline.is_some_and(|deadline| deadline <= now) {
            return false;
        }
        // This runs on the timer itself; discard the handle without aborting this task.
        self.timer.take();
        let _ = self.set(false, None, now);
        true
    }

    fn status(&self) -> AwakeStatus {
        AwakeStatus {
            enabled: self.request.is_some(),
            remaining_ms: self.deadline.map(|d| d.saturating_duration_since(Instant::now()).as_millis() as u64),
        }
    }
}

static REQUEST: Mutex<Lease> = Mutex::new(Lease::new());

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
    REQUEST.lock().request.is_some()
}

fn publish(app: &AppHandle, failed: bool) {
    let ui = app.clone();
    let _ = app.run_on_main_thread(move || {
        let status = get_background_awake_status();
        super::tray::update_background_awake(status.enabled, status.remaining_ms.is_some(), failed && !status.enabled);
        let _ = ui.emit(EVENT, status.enabled);
        let _ = ui.emit(STATUS_EVENT, status);
    });
}

fn update(app: &AppHandle, enabled: Option<bool>, minutes: Option<u16>) -> Result<bool, String> {
    let result = {
        let mut lease = REQUEST.lock();
        let enable = enabled.unwrap_or(lease.request.is_none());
        lease.set(enable, minutes, Instant::now()).map(|(enabled, deadline)| {
            if let Some(deadline) = deadline {
                let app = app.clone();
                let generation = lease.generation;
                lease.timer = Some(tauri::async_runtime::spawn(async move {
                    tokio::time::sleep_until(deadline.into()).await;
                    let expired = REQUEST.lock().expire(generation, Instant::now());
                    if expired {
                        publish(&app, false);
                    }
                }));
            }
            enabled
        })
    };
    publish(app, result.is_err());
    result
}

pub(crate) fn toggle_from_tray(app: &AppHandle) {
    if let Err(error) = update(app, None, None) {
        crate::log_warn!("AWAKE", "{error}");
    }
}

pub(crate) fn start_timed_from_tray(app: &AppHandle, minutes: u16) {
    if let Err(error) = update(app, Some(true), Some(minutes)) {
        crate::log_warn!("AWAKE", "{error}");
    }
}

pub(crate) fn release() {
    let _ = REQUEST.lock().set(false, None, Instant::now());
}

pub(crate) fn release_for_sleep() {
    if !active() {
        return;
    }
    // Explicit sleep is not an error. The next session must opt in again.
    if let Some(app) = super::webview_orchestrator::WebviewOrchestrator::global().app() {
        let _ = update(&app, Some(false), None);
    } else {
        release();
    }
}

#[tauri::command]
pub fn get_background_awake() -> bool {
    active()
}

#[tauri::command]
pub fn get_background_awake_status() -> AwakeStatus {
    REQUEST.lock().status()
}

#[tauri::command]
pub fn set_background_awake(window: tauri::WebviewWindow, enable: bool, minutes: Option<u16>) -> Result<bool, String> {
    use tauri::Manager;
    if window.label() != "main" {
        return Err("请在 OpenRevo 主窗口或托盘设置后台保活".into());
    }
    update(window.app_handle(), Some(enable), minutes)
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
    fn superseded_timer_cannot_release_new_request_and_invalid_duration_preserves_it() {
        let now = Instant::now();
        let mut lease = Lease::new();
        let (_, original) = lease.set(true, Some(1), now).unwrap();
        let old_generation = lease.generation;
        let (_, extended) = lease.set(true, Some(2), now).unwrap();
        assert!(lease.set(true, Some(0), now).is_err());
        assert!(lease.set(true, Some(721), now).is_err());
        let current_generation = lease.generation;
        assert!(!lease.expire(old_generation, original.unwrap()));
        assert!(!lease.expire(current_generation, original.unwrap()));
        assert!(lease.status().enabled);
        assert!(lease.expire(current_generation, extended.unwrap()));
        assert!(!lease.status().enabled);
        assert!(!lease.expire(current_generation, extended.unwrap()));
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
