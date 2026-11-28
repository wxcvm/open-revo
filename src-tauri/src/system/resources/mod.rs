mod history;
mod model;
#[cfg(windows)]
mod windows;

use self::history::History;
use self::model::Values;
pub use self::model::{Minute, Snapshot};
use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const RECENT_MS: u64 = 600_000;
const LEASE: Duration = Duration::from_secs(6);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Foreground,
    Background,
    Paused,
    Idle,
}
impl Mode {
    fn interval(self) -> Option<Duration> {
        match self {
            Self::Foreground => Some(Duration::from_secs(2)),
            Self::Background => Some(Duration::from_secs(10)),
            _ => None,
        }
    }
}

struct Frame {
    time_ms: u64,
    summary: Values,
    items: HashMap<String, [Option<f64>; 4]>,
    gap_before: bool,
}
impl Frame {
    fn capture(snapshot: &Snapshot, gap_before: bool) -> Self {
        let items = snapshot
            .processes
            .iter()
            .filter_map(|p| {
                Some((
                    p.key.clone()?,
                    [
                        p.cpu_percent,
                        p.private_resident.map(|n| n as f64),
                        None,
                        None,
                    ],
                ))
            })
            .chain(
                snapshot
                    .disks
                    .iter()
                    .map(|d| (d.key.clone(), [d.read, d.write, d.read_iops, d.write_iops])),
            )
            .chain(
                snapshot
                    .interfaces
                    .iter()
                    .map(|i| (i.key.clone(), [i.rx, i.tx, None, None])),
            )
            .collect();
        Self {
            time_ms: snapshot.time_ms,
            summary: snapshot.summary,
            items,
            gap_before,
        }
    }
}

#[derive(Serialize)]
struct Point {
    time_ms: u64,
    summary: Values,
    selected: [Option<f64>; 4],
    gap_before: bool,
}

#[derive(Serialize)]
pub struct Live {
    snapshot: Option<Snapshot>,
    recent: Vec<Point>,
    history: HistoryStatus,
    mode: Mode,
}
#[derive(Serialize)]
pub struct HistoryStatus {
    enabled: bool,
    error: Option<String>,
}

#[derive(Default)]
struct State {
    leases: [Option<Instant>; 2],
    snapshot: Option<Snapshot>,
    frames: VecDeque<Frame>,
}
struct Monitor {
    state: Mutex<State>,
    signal: Condvar,
    history: Mutex<History>,
}
static MONITOR: OnceLock<Monitor> = OnceLock::new();

pub fn start(dir: PathBuf) {
    if MONITOR
        .set(Monitor {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
            history: Mutex::new(History::open(dir)),
        })
        .is_err()
    {
        return;
    }
    #[cfg(windows)]
    if let Err(error) = std::thread::Builder::new()
        .name("resource-sampler".into())
        .spawn(worker)
    {
        crate::log_error!("RESOURCES", "Failed to start resource sampler: {error}");
    }
}

fn monitor() -> Result<&'static Monitor, String> {
    MONITOR.get().ok_or_else(|| "资源监控尚未初始化".into())
}
fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn gates(state: &State, enabled: bool, now: Instant) -> (Mode, u32) {
    let (visible, suspended, epoch) = super::telemetry::resource_gates();
    let mode = if suspended {
        Mode::Paused
    } else if state.leases.iter().enumerate().any(|(i, last)| {
        (i == 1 || visible) && last.is_some_and(|last| now.saturating_duration_since(last) < LEASE)
    })
    {
        Mode::Foreground
    } else if enabled {
        Mode::Background
    } else {
        Mode::Idle
    };
    (mode, epoch)
}
pub(crate) fn wake() {
    if let Some(m) = MONITOR.get() {
        let _state = m.state.lock();
        m.signal.notify_one();
    }
}
fn status(h: &History) -> HistoryStatus {
    HistoryStatus {
        enabled: h.enabled,
        error: h.error.clone(),
    }
}
pub fn live(selected: Option<String>, manager: bool) -> Result<Live, String> {
    if selected.as_ref().is_some_and(|s| s.len() > 512) {
        return Err("资源标识过长".into());
    }
    let m = monitor()?;
    let history = status(&m.history.lock());
    let mut state = m.state.lock();
    let now = Instant::now();
    let wall = wall_ms();
    let (before, _) = gates(&state, history.enabled, now);
    state.leases[usize::from(manager)] = Some(now);
    let (mode, _) = gates(&state, history.enabled, now);
    if mode != before {
        m.signal.notify_one();
    }
    let snapshot = state
        .snapshot
        .as_ref()
        .filter(|s| {
            mode == Mode::Foreground && wall.saturating_sub(s.time_ms) <= LEASE.as_millis() as u64
        })
        .cloned();
    let recent = state
        .frames
        .iter()
        .filter(|f| wall.saturating_sub(f.time_ms) <= RECENT_MS)
        .map(|frame| Point {
            time_ms: frame.time_ms,
            summary: frame.summary,
            gap_before: frame.gap_before,
            selected: selected
                .as_ref()
                .and_then(|id| frame.items.get(id))
                .copied()
                .unwrap_or([None; 4]),
        })
        .collect();
    Ok(Live {
        snapshot,
        recent,
        history,
        mode,
    })
}
pub fn release_view(manager: bool) -> Result<(), String> {
    let m = monitor()?;
    let mut state = m.state.lock();
    state.leases[usize::from(manager)] = None;
    if state.leases.iter().all(Option::is_none) { state.snapshot = None; }
    m.signal.notify_one();
    Ok(())
}
#[derive(Clone, Copy, Serialize)]
pub struct Peak { pub time_ms: u64, pub metric: &'static str, pub value: f64 }

pub fn recent_peaks() -> Result<Vec<Peak>, String> {
    let m = monitor()?;
    let now = wall_ms();
    let mut peaks = std::collections::BTreeMap::<(u64, usize), Peak>::new();
    for frame in &m.state.lock().frames {
        if now.saturating_sub(frame.time_ms) > RECENT_MS { continue; }
        let net = frame.summary[8].zip(frame.summary[9]).map(|(a, b)| a + b);
        let disk = frame.summary[4].zip(frame.summary[5]).map(|(a, b)| a + b);
        for (i, (metric, value)) in [("memory", frame.summary[0]), ("cpu", frame.summary[3]),
            ("disk", disk), ("network", net)].into_iter().enumerate() {
            if let Some(n) = value.filter(|n| n.is_finite() && *n > 0.0) {
                let peak = peaks.entry((frame.time_ms / 60_000, i)).or_insert(Peak { time_ms: frame.time_ms, metric, value: n });
                if n > peak.value { *peak = Peak { time_ms: frame.time_ms, metric, value: n }; }
            }
        }
    }
    Ok(peaks.into_values().rev().collect())
}

pub fn read_history(hours: u16) -> Result<Vec<Minute>, String> {
    monitor()?.history.lock().read(wall_ms(), hours)
}
pub fn change_history(enabled: bool) -> Result<HistoryStatus, String> {
    let status = {
        let mut history = monitor()?.history.lock();
        history.set_enabled(enabled)?;
        history.maintain(wall_ms());
        status(&history)
    };
    wake();
    Ok(status)
}
pub fn clear_history() -> Result<HistoryStatus, String> {
    let mut history = monitor()?.history.lock();
    history.clear()?;
    Ok(status(&history))
}
pub fn history_csv(hours: u16) -> Result<String, String> {
    history::csv(&read_history(hours)?)
}
pub fn flush_on_exit() {
    if let Some(m) = MONITOR.get() {
        m.history.lock().flush(wall_ms());
    }
}

#[cfg(windows)]
fn worker() {
    let m = MONITOR.get().unwrap();
    let mut sampler = windows::Sampler::new();
    let mut previous_gate = (Mode::Idle, 0);
    let mut previous_sample: Option<(Instant, u64)> = None;
    let mut due = Instant::now();
    let mut maintenance = due;
    loop {
        let now = Instant::now();
        let (enabled, mode, epoch) = {
            let enabled = m.history.lock().enabled;
            let state = m.state.lock();
            let (mode, epoch) = gates(&state, enabled, now);
            (enabled, mode, epoch)
        };
        if (mode, epoch) != previous_gate {
            sampler.reset();
            previous_sample = None;
            m.history.lock().disconnect();
            m.state.lock().snapshot = None;
            previous_gate = (mode, epoch);
            due = now;
        }
        if mode != Mode::Paused && now >= maintenance {
            m.history.lock().maintain(wall_ms());
            maintenance = now + Duration::from_secs(30);
        }
        if let Some(interval) = mode.interval().filter(|_| now >= due) {
            let wall = wall_ms();
            let detail = mode == Mode::Foreground;
            let snapshot = sampler.collect(now, wall, detail);
            let elapsed =
                previous_sample.map(|(before, _)| now.duration_since(before).as_millis() as u64);
            due = now + interval;
            let mut history = m.history.lock();
            let mut state = m.state.lock();
            if gates(&state, history.enabled, Instant::now()) == (mode, epoch) {
                if enabled && history.enabled {
                    history.observe(wall, elapsed, snapshot.summary);
                    // Finalize at minute rollover without writing each individual sample.
                    if previous_sample.is_some_and(|(_, before)| before / 60_000 != wall / 60_000) {
                        drop(state);
                        history.maintain(wall);
                        state = m.state.lock();
                    }
                }
                if detail && gates(&state, history.enabled, Instant::now()) == (mode, epoch) {
                    let gap = previous_sample.is_none_or(|(_, before)| {
                        !(500..=6_000).contains(&wall.saturating_sub(before))
                    });
                    if state.frames.back().is_some_and(|f| f.time_ms >= wall) {
                        state.frames.clear();
                    }
                    while state
                        .frames
                        .front()
                        .is_some_and(|f| wall.saturating_sub(f.time_ms) > RECENT_MS)
                        || state.frames.len() >= 310
                    {
                        state.frames.pop_front();
                    }
                    state.frames.push_back(Frame::capture(&snapshot, gap));
                    state.snapshot = Some(snapshot);
                }
                previous_sample = Some((now, wall));
            }
            continue;
        }
        let state = m.state.lock();
        let next_mark = match mode {
            Mode::Paused => now + Duration::from_secs(30),
            Mode::Idle => maintenance,
            Mode::Background | Mode::Foreground => due.min(maintenance),
        };
        let mut state = state;
        let delay = if mode == Mode::Foreground {
            state
                .leases
                .iter()
                .flatten()
                .copied()
                .max()
                .map(|lease| (lease + LEASE).min(next_mark))
                .unwrap_or(now)
        } else {
            next_mark
        };
        // Check the native gate again before sleeping (visibility and power callbacks wake it).
        if gates(&state, enabled, Instant::now()) == (mode, epoch) {
            m.signal.wait_for(
                &mut state,
                delay
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(10)),
            );
        }
    }
}

#[cfg(all(test, windows))]
#[test]
#[ignore = "read-only live Windows counter acceptance"]
fn resource_live_windows_probe() {
    let mut sampler = windows::Sampler::new();
    let a = sampler.collect(Instant::now(), wall_ms(), true);
    let pid = std::process::id();
    let before = a
        .processes
        .iter()
        .find(|p| p.pid == pid)
        .expect("own process");
    let mut allocated = vec![0u8; 96 * 1024 * 1024];
    for p in allocated.chunks_mut(4096) {
        p[0] = 1;
    }
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        std::hint::black_box(allocated[123_000]);
        std::hint::spin_loop();
    }
    let b = sampler.collect(Instant::now(), wall_ms(), true);
    std::hint::black_box(&allocated);
    let after = b
        .processes
        .iter()
        .find(|p| p.pid == pid)
        .expect("own process after workload");
    let resident_delta = after
        .private_resident
        .unwrap()
        .saturating_sub(before.private_resident.unwrap());
    let own_cpu = after.cpu_percent.unwrap();
    println!("controlled private resident delta={resident_delta} bytes; one busy thread used {own_cpu:.2}% of whole machine");
    assert!(resident_delta >= 80 * 1024 * 1024);
    assert!((0.05..=130.0 / f64::from(b.logical_cpus)).contains(&own_cpu));
    assert_eq!(before.key, after.key);
    drop(allocated);
    let known_cpu = b
        .processes
        .iter()
        .filter(|p| p.cpu_percent.is_some())
        .count();
    println!("source={:?} logical={} processes={} cpu_readable={} proc_mem={:?} installed={:?} visible={:?} available={:?} standby={:?} free={:?} disk_count={} net_physical={} elapsed_ms={} rates={:?}",
        b.process_source, b.logical_cpus, b.processes.len(), known_cpu, b.memory.private_resident_sum,
        b.memory.installed, b.memory.visible,b.memory.available,b.memory.standby,b.memory.free_zero,
        b.disks.len(), b.interfaces.iter().filter(|i| i.physical && i.up).count(), b.sampled_in_ms, b.summary);
    assert!(a.processes.iter().all(|p| p.cpu_percent.is_none()));
    assert!(b.memory.visible.is_some() && b.memory.private_readable > 0 && known_cpu > 0);
    assert!(b.summary[3].is_some() && b.summary[4].is_some());
    assert!(!b.disks.is_empty() && b.net_available);
    sampler.reset();
    let _ = sampler.collect(Instant::now(), wall_ms(), false);
    std::thread::sleep(Duration::from_secs(2));
    let c = sampler.collect(Instant::now(), wall_ms(), false);
    assert!(
        c.processes.is_empty()
            && c.disks.is_empty()
            && c.interfaces.is_empty()
            && c.summary[3].is_some()
    );
}
