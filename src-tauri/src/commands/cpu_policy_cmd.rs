//! CPU policy commands: query, apply and reset to the in-memory session snapshot.
//!
//! The baseline is deliberately memory-only. Nothing about CPU capabilities or
//! values is persisted to configuration. Apply/reset serialize on the existing
//! CONTROL then ENERGY locks, plus a CPU lock, and re-apply the SAME captured
//! plan after saving values so Windows makes them effective. They never switch
//! plans. A reset only targets the plan GUID that was active when the session
//! first read it.

use crate::system::cpu_policy::{self, Outcome, Snapshot, Update};
use crate::system::cpu_policy_win::{self, WindowsPowrProf};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;

static CPU_POLICY_LOCK: Mutex<()> = Mutex::new(());

fn sessions() -> &'static Mutex<HashMap<String, Snapshot>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, Snapshot>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Keep lock order CONTROL -> ENERGY -> CPU everywhere. The CPU lock also makes
/// apply/reset mutually exclusive with each other and with query readback.
fn with_cpu_serialization<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _control = crate::system::vendor_bus::CONTROL_MODE_LOCK.lock();
    let _energy = crate::system::vendor_bus::ENERGY_LOCK.lock();
    let _cpu = CPU_POLICY_LOCK.lock();
    f()
}

/// Keep the first snapshot seen for each exact plan GUID. Later queries do not
/// overwrite the baseline, so reset always means "start of this app session".
fn remember(snapshot: &Snapshot) {
    let mut sessions = sessions().lock();
    sessions
        .entry(snapshot.plan_guid.clone())
        .or_insert_with(|| snapshot.clone());
}

#[tauri::command]
pub fn get_cpu_policy() -> Result<Snapshot, String> {
    let _cpu = CPU_POLICY_LOCK.lock();
    let snapshot = cpu_policy_win::query_snapshot()?;
    remember(&snapshot);
    Ok(snapshot)
}

#[tauri::command]
pub fn apply_cpu_policy(
    request: Update,
    expected_plan_guid: Option<String>,
) -> Result<Outcome, String> {
    with_cpu_serialization(|| {
        let snapshot = cpu_policy_win::query_snapshot()?;
        if let Some(expected) = expected_plan_guid {
            if expected != snapshot.plan_guid {
                return Err(format!(
                    "The active Windows power plan changed from {expected} to {}; no CPU policy write was attempted",
                    snapshot.plan_guid
                ));
            }
        }
        remember(&snapshot);
        let mut backend = WindowsPowrProf;
        cpu_policy::execute(&mut backend, &snapshot, &request)
    })
}

#[tauri::command]
pub fn reset_cpu_policy_session() -> Result<Outcome, String> {
    with_cpu_serialization(|| {
        let current = cpu_policy_win::query_snapshot()?;
        let baseline = sessions().lock().get(&current.plan_guid).cloned();
        let baseline = baseline.ok_or_else(|| {
            format!(
                "No CPU policy session snapshot exists for active Windows power plan {}; query the policy first. No value was changed.",
                current.plan_guid
            )
        })?;
        let update = cpu_policy::restore_update(&baseline);
        let mut backend = WindowsPowrProf;
        cpu_policy::execute(&mut backend, &current, &update)
    })
}
