use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};
use std::time::Instant;
use parking_lot::Mutex;

static BOOT_START_INSTANT: OnceLock<Instant> = OnceLock::new();
#[derive(Default)]
struct BootTrace {
    pending: String,
    file: Option<File>,
}
static BOOT_TRACE: LazyLock<Mutex<BootTrace>> = LazyLock::new(|| Mutex::new(BootTrace::default()));
static BOOT_COMPLETED: AtomicBool = AtomicBool::new(false);

pub fn get_boot_log_path() -> PathBuf {
    crate::system::config::get_config_dir().join("boot.log")
}

/// Initialize the Boot Trace system at the very beginning of main().
/// Buffer early milestones until Tauri has admitted this instance as primary.
pub fn init_boot_trace() {
    let start = Instant::now();
    let _ = BOOT_START_INSTANT.set(start);

    let now_str = chrono_or_fallback_now();
    let args: Vec<String> = std::env::args().collect();
    let pid = std::process::id();
    let current_exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    BOOT_TRACE.lock().pending = format!(
            "================================================================================\n\
             OpenRevo Boot Trace Log (Dedicated Startup Diagnostics)\n\
             Boot Time: {}\n\
             Binary:    {}\n\
             PID:       {}\n\
             Arguments: {:?}\n\
             ================================================================================\n",
            now_str, current_exe, pid, args
    );

    boot_log("INIT", &format!("Boot stopwatch initialized (PID: {})", std::process::id()));
}

/// Only Tauri's primary setup reaches this call. Secondary tray invocations
/// must not truncate the boot log while the original process still writes it.
pub fn open_primary_boot_log() {
    let mut state = BOOT_TRACE.lock();
    if state.file.is_some() {
        return;
    }
    match OpenOptions::new().create(true).write(true).truncate(true).open(get_boot_log_path()) {
        Ok(mut file) => {
            let _ = file.write_all(state.pending.as_bytes());
            let _ = file.flush();
            state.pending.clear();
            state.file = Some(file);
        }
        Err(error) => eprintln!("[BOOT] Unable to open primary boot trace: {error}"),
    }
}

fn chrono_or_fallback_now() -> String {
    let now = std::time::SystemTime::now();
    let duration = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs();
    format!("UNIX Epoch: {}s", secs)
}

/// Record a boot milestone with relative millisecond timestamp.
/// Persisted immediately to boot.log with zero disk buffering.
pub fn boot_log(tag: &str, message: &str) {
    let start = BOOT_START_INSTANT.get_or_init(Instant::now);
    let elapsed = start.elapsed();
    let ms = elapsed.as_millis();

    let line = format!("[+{:>5}ms] [{:<8}] {}\n", ms, tag, message);

    // 1. Output to console
    print!("[BOOT] {}", line);

    // 2. Output to global logger
    crate::log_info!("BOOT", "[{}] {}", tag, message);

    // 3. Persist unconditionally to boot.log
    let mut state = BOOT_TRACE.lock();
    if let Some(ref mut f) = state.file {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    } else if state.pending.len() + line.len() <= 64 * 1024 {
        state.pending.push_str(&line);
    }
}

/// Execute a boot milestone scope, automatically recording its start, end, and duration.
pub fn boot_scope<T, F: FnOnce() -> T>(step_name: &str, f: F) -> T {
    boot_log("START", step_name);
    let start = Instant::now();
    let res = f();
    let duration = start.elapsed();
    boot_log("DONE", &format!("{} (took {}ms)", step_name, duration.as_millis()));
    res
}

/// Record backend/tray setup, which can finish without a main WebView.
pub fn record_stage0_ui_ready() {
    let total = BOOT_START_INSTANT.get().map(|s| s.elapsed().as_millis()).unwrap_or(0);
    boot_log(
        "STAGE0_OK",
        &format!("Backend and tray ready (+{}ms). The main WebView handshakes after it is opened.", total),
    );
}

/// Record that frontend React/Vue application mounted, executed first render, and established two-way IPC handshake.
#[tauri::command]
pub fn frontend_ready(window: tauri::WebviewWindow) -> Result<u128, String> {
    if window.label() != "main" {
        return Err("The main window owns the startup handshake".into());
    }
    let total = BOOT_START_INSTANT.get().map(|s| s.elapsed().as_millis()).unwrap_or(0);
    boot_log(
        "FE_READY",
        &format!("Frontend DOM Mounted & Handshake Confirmed (+{}ms). WebView2 message loop fully responsive!", total),
    );
    mark_boot_completed();
    crate::system::webview_orchestrator::WebviewOrchestrator::global().notify_main_ready();
    crate::system::telemetry::set_main_window_visible(
        window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(true),
    );

    Ok(total)
}

/// Record asynchronous GPU cold-probe completion with precise duration.
pub fn record_gpu_probe_milestone(name: &str, base_w: u16, max_w: u16, duration_ms: u128) {
    boot_log(
        "GPU_DONE",
        &format!("Async GPU Cold Probe Completed in {}ms: {} (Base: {}W, Max: {}W)", duration_ms, name, base_w, max_w),
    );
}

/// Mark boot sequence completed and Tauri event loop active.
pub fn mark_boot_completed() {
    BOOT_COMPLETED.store(true, Ordering::SeqCst);
    let total = BOOT_START_INSTANT.get().map(|s| s.elapsed().as_millis()).unwrap_or(0);
    boot_log(
        "BOOT_OK",
        &format!("=== All Boot Tasks Finished in {}ms! Full System Operational. ===", total),
    );
}

/// Check if boot sequence has completed
pub fn is_boot_completed() -> bool {
    BOOT_COMPLETED.load(Ordering::Relaxed)
}

/// Retrieve the current boot log contents for frontend diagnostics view
#[tauri::command]
pub fn get_boot_log() -> Result<String, String> {
    let path = get_boot_log_path();
    std::fs::read_to_string(&path).map_err(|e| format!("Failed to read boot log: {}", e))
}
