//! Process-wide ADLX entry points used by the Tauri commands.
//!
//! Why a dedicated thread: the ADLX runtime is initialized and terminated per process, and the live
//! driver test proved it is thread-affine - two worker threads each doing
//! `ADLXInitialize`/`ADLXTerminate` (even when serialized by a mutex) faulted with
//! STATUS_ACCESS_VIOLATION. So one worker thread owns the ADLX session for the whole process:
//!
//! * it opens the session lazily on the first request and keeps it open (no init/terminate churn),
//! * every request runs on that thread, which serializes ADLX access,
//! * when the app invokes `shutdown` (or all senders drop) it releases every interface first, then calls
//!   `ADLXTerminate`, then unloads the DLL - all on the same thread that initialized ADLX.
//!
//! The session ledger (settings this app changed plus their pre-change values) lives on that thread
//! too, so it survives across commands without keeping any driver handle in the UI layer.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::engine::*;
use super::session::AdlxSession;

enum Job {
    State(SyncSender<DriverGraphicsState>),
    Metrics,
    SetToggle(GfxFeature, bool, SyncSender<Result<UpdateReport, String>>),
    SetChillFps(i32, i32, SyncSender<Result<UpdateReport, String>>),
    SetFrtcFps(i32, SyncSender<Result<UpdateReport, String>>),
    Restore(SyncSender<Result<RestoreReport, String>>),
    ChangedFields(SyncSender<Vec<String>>),
    Shutdown(SyncSender<()>),
}

struct Request {
    job: Job,
    deadline: Instant,
}

struct AdlxWorker {
    tx: SyncSender<Request>,
}

static WORKER: OnceLock<AdlxWorker> = OnceLock::new();

fn worker() -> &'static AdlxWorker {
    WORKER.get_or_init(|| {
        // A bounded request queue prevents unlimited pending driver work. Replies have
        // their own channel, and expired queued jobs are never executed.
        let (tx, rx) = sync_channel::<Request>(1);
        std::thread::Builder::new()
            .name("adlx-driver".into())
            .spawn(move || adlx_thread_main(rx))
            .expect("failed to spawn the ADLX worker thread");
        AdlxWorker { tx }
    })
}

fn adlx_thread_main(rx: Receiver<Request>) {
    let mut session: Option<AdlxSession> = None;
    let mut ledger = Ledger::new();
    let mut last_error = session_error();
    let mut retry_after = Instant::now();

    while let Ok(request) = rx.recv() {
        if Instant::now() >= request.deadline {
            continue;
        }
        let job = request.job;
        if let Job::Shutdown(reply) = job {
            drop(session.take());
            let _ = reply.send(());
            break;
        }
        // Lazily open once; a failure is retried on the next request (the driver may appear later,
        // e.g. after a driver update) and reported honestly in the meantime.
        if session.is_none() && Instant::now() >= retry_after {
            session = match AdlxSession::open() {
                Ok(s) => Some(s),
                Err(reason) => {
                    log_adlx_unavailable(&reason);
                    last_error = reason;
                    retry_after = Instant::now() + Duration::from_secs(10);
                    None
                }
            };
        }

        match job {
            Job::Metrics => {
                if let Some(s) = session.as_ref() {
                    *metric_cache().lock() = Some((Instant::now(), s.metrics()));
                }
            }
            Job::State(reply) => {
                let state = match session.as_ref() {
                    Some(s) => read_with_ledger(s, &mut ledger),
                    None => DriverGraphicsState::unavailable(last_error.clone()),
                };
                let _ = reply.send(state);
            }
            Job::SetToggle(feature, enabled, reply) => {
                let result = with_controller(&session, &last_error, &mut ledger, |c| {
                    c.apply_toggle(feature, enabled)
                });
                let _ = reply.send(result);
            }
            Job::SetChillFps(min, max, reply) => {
                let result = with_controller(&session, &last_error, &mut ledger, |c| {
                    c.apply_chill_fps(min, max)
                });
                let _ = reply.send(result);
            }
            Job::SetFrtcFps(fps, reply) => {
                let result = with_controller(&session, &last_error, &mut ledger, |c| {
                    c.apply_frtc_fps(fps)
                });
                let _ = reply.send(result);
            }
            Job::Restore(reply) => {
                let result =
                    with_controller(&session, &last_error, &mut ledger, |c| c.restore_session());
                let _ = reply.send(result);
            }
            Job::Shutdown(_) => unreachable!(),
            Job::ChangedFields(reply) => {
                let _ = reply.send(ledger.changed_keys());
            }
        }
    }

    // Shutdown: this is the thread that called ADLXInitialize, and `AdlxSession::drop` releases all
    // interfaces, calls ADLXTerminate, then unloads the DLL.
    drop(session);
}

/// Kept for the "why is this panel empty" path; avoids pulling the logger into tests.
fn log_adlx_unavailable(reason: &str) {
    if std::env::var_os("OPENREVO_ADLX_QUIET").is_none() {
        crate::log_warn!("HAL", "ADLX graphics controls unavailable: {}", reason);
    }
}

fn session_error() -> String {
    "ADLX session is not available on this system".to_string()
}

fn read_with_ledger(session: &AdlxSession, ledger: &mut Ledger) -> DriverGraphicsState {
    let mut controller = GfxController::new(session.backend(), std::mem::take(ledger));
    let state = controller.state();
    *ledger = controller.into_ledger();
    state
}

/// Runs `op` with the process ledger swapped into a fresh controller, then stores it back.
///
/// The closure is monomorphic over the borrow of `session`, so the drop order stays explicit:
/// controller -> backend -> (session, owned by the worker).
fn with_controller<T>(
    session: &Option<AdlxSession>,
    last_error: &str,
    ledger: &mut Ledger,
    op: impl FnOnce(&mut GfxController<super::session::AdlxBackend<'_>>) -> T,
) -> Result<T, String> {
    let session = session.as_ref().ok_or_else(|| last_error.to_string())?;
    let mut controller = GfxController::new(session.backend(), std::mem::take(ledger));
    let result = op(&mut controller);
    *ledger = controller.into_ledger();
    Ok(result)
}

fn dispatch<T>(make: impl FnOnce(SyncSender<T>) -> Job) -> Result<T, String> {
    let (tx, rx) = sync_channel::<T>(0);
    worker()
        .tx
        .try_send(Request {
            job: make(tx),
            deadline: Instant::now() + Duration::from_secs(15),
        })
        .map_err(|e| format!("ADLX worker busy or disconnected: {e}"))?;
    rx.recv_timeout(Duration::from_secs(15))
        .map_err(|e| format!("ADLX response not confirmed ({e}); an executing driver call cannot be cancelled. Re-read before retrying; restart OpenRevo if the driver remains unresponsive"))
}

/// Read-only: never initializes anything if the DLL/driver is missing; the state then carries
/// `available: false` and the reason.
pub fn read_state() -> DriverGraphicsState {
    dispatch(Job::State).unwrap_or_else(|reason| DriverGraphicsState::unavailable(reason))
}

pub fn set_toggle(feature: GfxFeature, enabled: bool) -> Result<UpdateReport, String> {
    dispatch(|reply| Job::SetToggle(feature, enabled, reply))?
}

pub fn set_chill_fps(min: i32, max: i32) -> Result<UpdateReport, String> {
    dispatch(|reply| Job::SetChillFps(min, max, reply))?
}

pub fn set_frtc_fps(fps: i32) -> Result<UpdateReport, String> {
    dispatch(|reply| Job::SetFrtcFps(fps, reply))?
}

pub fn restore_session() -> Result<RestoreReport, String> {
    dispatch(Job::Restore)?
}

/// Session bookkeeping on the owning worker (used by tests and diagnostics).
/// A first request can lazily initialize the worker's driver session; no setting is written.
pub fn changed_fields() -> Vec<String> {
    dispatch(Job::ChangedFields).unwrap_or_default()
}

/// Absolute path this module would load; exposed for diagnostics/tests.
pub fn dll_path() -> Result<String, String> {
    super::session::system_adlx_dll_path()
}

/// Do not initialize ADLX merely to shut it down. Release on its owning thread.
pub fn shutdown() {
    if let Some(worker) = WORKER.get() {
        let (tx, rx) = sync_channel(0);
        if worker
            .tx
            .try_send(Request {
                job: Job::Shutdown(tx),
                deadline: Instant::now() + Duration::from_secs(2),
            })
            .is_ok()
        {
            let _ = rx.recv_timeout(Duration::from_secs(2));
        }
    }
}

fn metric_cache() -> &'static parking_lot::Mutex<Option<(Instant, super::metrics::DriverMetrics)>> {
    static CACHE: OnceLock<parking_lot::Mutex<Option<(Instant, super::metrics::DriverMetrics)>>> =
        OnceLock::new();
    CACHE.get_or_init(|| parking_lot::Mutex::new(None))
}
/// Nonblocking telemetry: enqueue a read at most once a second. Never wait on the driver
/// in the heartbeat thread, never reuse samples older than five seconds or another adapter.
pub fn cached_metrics(name: &str) -> Option<super::metrics::DriverMetrics> {
    if !name.to_ascii_lowercase().contains("radeon") {
        return None;
    }
    static REQUEST: parking_lot::Mutex<Option<Instant>> = parking_lot::Mutex::new(None);
    let mut request = REQUEST.lock();
    if request.map_or(true, |at| at.elapsed() >= Duration::from_secs(1)) {
        *request = Some(Instant::now());
        let _ = worker().tx.try_send(Request {
            job: Job::Metrics,
            deadline: Instant::now() + Duration::from_secs(2),
        });
    }
    drop(request);
    metric_cache()
        .lock()
        .as_ref()
        .filter(|(at, m)| {
            at.elapsed() < Duration::from_secs(5) && m.gpu_name.eq_ignore_ascii_case(name)
        })
        .map(|(_, m)| m.clone())
}
