//! P916F-STX battery charge-limit HAL.
//!
//! This module is scoped to the exact MECHREVO Xingyao 14 `P916F-STX` DMI
//! manifest and BIOS 1.15. It talks to the EC through the already-installed
//! signed inpoutx64 driver and the ITE PMC2 ports only. It never installs,
//! starts, replaces or deletes a driver.
//!
//! Safety model:
//! * the caller must pass a confirmed [`IdentityCheck`] before any port I/O;
//! * every transaction is serialised by a process-local `parking_lot::Mutex`
//!   and the cross-process `Global\OpenRevo.P916F.PMC2` mutex;
//! * only PMC2 ports 0x68 and 0x6C are ever read or written;
//! * IBF/OBF waits are bounded (200 ms); 0xFF or a stale OBF is refused rather
//!   than drained blindly;
//! * setters consume the firmware echo and then independently requery;
//! * an apply is transactional: it starts from the UI's exact prior snapshot,
//!   verifies every step, and conditionally restores only values this call wrote;
//! * T2 is pinned to 100 for app writes. T2 below 100 is an active-discharge
//!   boundary on this platform, not a conventional resume threshold.
//!
//! Disable/reset (`F1 10`) was locally verified on BIOS 1.15 and erases both thresholds
//! on the EC. Restoring an earlier enabled policy therefore requires an explicit
//! new apply; it is not a "resume" operation.

use serde::{Deserialize, Serialize};

use super::vault::p916f_pmc2 as spec;

// ---------------------------------------------------------------------------------------------
// Identity gate and serialisable state
// ---------------------------------------------------------------------------------------------

/// Result of the exact model + BIOS guard, computed before any hardware I/O.
/// Fields are private so a caller cannot fabricate a confirmed gate; use
/// [`verify_identity`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCheck {
    matched: bool,
    detail: Option<String>,
}

impl IdentityCheck {
    fn rejected(detail: impl Into<String>) -> Self {
        IdentityCheck {
            matched: false,
            detail: Some(detail.into()),
        }
    }

    fn confirmed() -> Self {
        IdentityCheck {
            matched: true,
            detail: None,
        }
    }

    pub fn is_matched(&self) -> bool {
        self.matched
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

/// Exact DMI manifest id + registry BIOS guard. This is pure: no I/O at all.
pub fn verify_identity(manifest_id: Option<&str>, bios_version: &str) -> IdentityCheck {
    match manifest_id {
        Some(id) if id == spec::MODEL_ID => {}
        Some(other) => {
            return IdentityCheck::rejected(format!(
                "Matched model is '{other}', not {}; P916F-STX PMC2 is disabled",
                spec::MODEL_ID
            ))
        }
        None => {
            return IdentityCheck::rejected(
                "No declarative DMI manifest matched this machine; P916F-STX PMC2 is disabled",
            )
        }
    }

    let bios = bios_version.trim();
    if bios != spec::BIOS_VERSION {
        return IdentityCheck::rejected(format!(
            "BIOSVersion is '{}', not exactly '{}'; P916F-STX PMC2 is disabled",
            if bios.is_empty() { "unreadable" } else { bios },
            spec::BIOS_VERSION
        ));
    }

    IdentityCheck::confirmed()
}

/// Exact PMC2-visible charge-limit state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeChargeSnapshot {
    pub enabled: u8,
    pub threshold1: u8,
    pub threshold2: u8,
}

impl NativeChargeSnapshot {
    pub fn is_disabled_erased(self) -> bool {
        self.enabled == spec::PMC2_ENABLED_OFF && self.threshold1 == 0 && self.threshold2 == 0
    }

    pub fn describe(self) -> String {
        format!(
            "enabled={} T1={} T2={}",
            self.enabled, self.threshold1, self.threshold2
        )
    }
}

/// UI-facing query result. Unknown reads stay `None`; they are never rendered
/// as zero. `matched=false` means the panel must stay hidden.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChargeState {
    pub matched: bool,
    pub available: bool,
    pub reason: Option<String>,
    pub enabled: Option<u8>,
    pub threshold1: Option<u8>,
    pub threshold2: Option<u8>,
}

impl ChargeState {
    fn from_gate(gate: &IdentityCheck) -> Self {
        ChargeState {
            matched: gate.matched,
            available: false,
            reason: gate.detail.clone(),
            enabled: None,
            threshold1: None,
            threshold2: None,
        }
    }

    fn unavailable(reason: impl Into<String>) -> Self {
        ChargeState {
            matched: true,
            available: false,
            reason: Some(reason.into()),
            enabled: None,
            threshold1: None,
            threshold2: None,
        }
    }

    fn from_snapshot(snapshot: NativeChargeSnapshot) -> Self {
        ChargeState {
            matched: true,
            available: true,
            reason: None,
            enabled: Some(snapshot.enabled),
            threshold1: Some(snapshot.threshold1),
            threshold2: Some(snapshot.threshold2),
        }
    }
}

/// Result of one apply attempt. `state` is a fresh readback, or `None` when even
/// the final query failed. `applied` is true only after an exact final readback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOutcome {
    pub applied: bool,
    pub failed: Option<String>,
    /// `None` = rollback was not needed. Otherwise a human-readable report.
    pub rollback: Option<String>,
    pub state: Option<NativeChargeSnapshot>,
}

impl ApplyOutcome {
    fn success(state: NativeChargeSnapshot) -> Self {
        ApplyOutcome {
            applied: true,
            failed: None,
            rollback: None,
            state: Some(state),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Request validation
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PolicyTarget {
    enabled: bool,
    threshold1: Option<u8>,
    threshold2: Option<u8>,
}

/// Validates the UI policy before any hardware access.
///
/// Enabled policies always target `(T1, 100)` with `T1` in `1..=99`. Numeric 100
/// is rejected because the unresolved `T1 == T2` case would be required; full
/// charge is applied by disabling the limit instead. T2 below 100 is never a
/// target because it would activate the documented active-discharge region.
pub fn validate_request(enabled: bool, threshold: u8) -> Result<(), String> {
    validate_target(enabled, threshold).map(|_| ())
}

fn validate_target(enabled: bool, threshold: u8) -> Result<PolicyTarget, String> {
    if enabled {
        if !(spec::CHARGE_T1_MIN..=spec::CHARGE_T1_MAX).contains(&threshold) {
            let reason = if threshold == spec::CHARGE_T2_FULL {
                "T1=100 requires the unresolved T1==T2 case; use disable for full charge"
            } else {
                "T1 must be a numeric cap in 1..=99"
            };
            return Err(format!(
                "Unsupported charge policy T1={threshold}: {reason}; no PMC2 write was attempted"
            ));
        }
        Ok(PolicyTarget {
            enabled: true,
            threshold1: Some(threshold),
            threshold2: Some(spec::CHARGE_T2_FULL),
        })
    } else {
        if threshold != spec::CHARGE_T2_FULL {
            return Err(format!(
                "Full charge is applied by disabling the limit (F1 10); threshold must be {} for disable, got {threshold}; no PMC2 write was attempted",
                spec::CHARGE_T2_FULL
            ));
        }
        Ok(PolicyTarget {
            enabled: false,
            threshold1: None,
            threshold2: None,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Pure PMC2 transaction engine (scripted in tools/native-system-tests)
// ---------------------------------------------------------------------------------------------

/// Minimal hardware surface used by the transaction engine. The Windows
/// implementation is below; tests provide a scripted implementation.
pub trait Pmc2Bus {
    fn read_port(&mut self, port: u16) -> Result<u8, String>;
    fn write_port(&mut self, port: u16, value: u8) -> Result<(), String>;
    fn now_ms(&self) -> u64;
    fn sleep_ms(&mut self, ms: u64);
}

fn check_port(port: u16) -> Result<(), String> {
    if port == spec::PMC2_DATA_PORT || port == spec::PMC2_COMMAND_PORT {
        Ok(())
    } else {
        Err(format!(
            "Refusing PMC2 access to non-allowlisted port 0x{port:02X}; only 0x68 and 0x6C are permitted"
        ))
    }
}

fn read_port(bus: &mut dyn Pmc2Bus, port: u16) -> Result<u8, String> {
    check_port(port)?;
    bus.read_port(port)
}

fn write_port(bus: &mut dyn Pmc2Bus, port: u16, value: u8) -> Result<(), String> {
    check_port(port)?;
    bus.write_port(port, value)
}

fn read_status(bus: &mut dyn Pmc2Bus) -> Result<u8, String> {
    let status = read_port(bus, spec::PMC2_COMMAND_PORT)?;
    if status == spec::PMC2_ABSENT {
        return Err(
            "PMC2 command/status port reads 0xFF (controller absent or not responding); no command was issued"
                .into(),
        );
    }
    Ok(status)
}

fn preflight(bus: &mut dyn Pmc2Bus) -> Result<(), String> {
    let status = read_status(bus)?;
    if status & spec::PMC2_STATUS_OBF != 0 {
        return Err(format!(
            "PMC2 reports a stale output byte (status 0x{status:02X}); refusing to drain it or issue a command"
        ));
    }
    Ok(())
}

fn wait_status(bus: &mut dyn Pmc2Bus, mask: u8, want_set: bool, what: &str) -> Result<u8, String> {
    let deadline = bus.now_ms().saturating_add(spec::PMC2_WAIT_TIMEOUT_MS);
    let mut polls = 0u32;

    loop {
        let status = read_status(bus)?;
        let matched = if want_set {
            status & mask == mask
        } else {
            status & mask == 0
        };
        if matched {
            return Ok(status);
        }

        polls += 1;
        if polls >= spec::PMC2_WAIT_MAX_POLLS || bus.now_ms() >= deadline {
            return Err(format!(
                "PMC2 {what} timed out after {} ms (last status 0x{status:02X}); no blind drain was attempted",
                spec::PMC2_WAIT_TIMEOUT_MS
            ));
        }
        bus.sleep_ms(spec::PMC2_WAIT_POLL_MS);
    }
}

fn send_returning(bus: &mut dyn Pmc2Bus, command: u8, data: u8) -> Result<u8, String> {
    preflight(bus)?;
    wait_status(
        bus,
        spec::PMC2_STATUS_IBF,
        false,
        "IBF clear before command",
    )?;
    write_port(bus, spec::PMC2_COMMAND_PORT, command)?;
    wait_status(bus, spec::PMC2_STATUS_IBF, false, "IBF clear after command")?;
    write_port(bus, spec::PMC2_DATA_PORT, data)?;
    wait_status(bus, spec::PMC2_STATUS_OBF, true, "OBF set for response")?;
    let value = read_port(bus, spec::PMC2_DATA_PORT)?;
    if value == spec::PMC2_ABSENT {
        return Err(
            "PMC2 response byte is 0xFF (stale/absent); refusing to accept it as a value".into(),
        );
    }
    Ok(value)
}

/// Enable/disable have no established response framing, so the caller must
/// reread state instead of consuming an OBF byte.
fn send_without_response(bus: &mut dyn Pmc2Bus, command: u8, data: u8) -> Result<(), String> {
    preflight(bus)?;
    wait_status(
        bus,
        spec::PMC2_STATUS_IBF,
        false,
        "IBF clear before command",
    )?;
    write_port(bus, spec::PMC2_COMMAND_PORT, command)?;
    wait_status(bus, spec::PMC2_STATUS_IBF, false, "IBF clear after command")?;
    write_port(bus, spec::PMC2_DATA_PORT, data)
}

pub struct Pmc2Engine<'a> {
    bus: &'a mut dyn Pmc2Bus,
}

impl<'a> Pmc2Engine<'a> {
    pub fn new(bus: &'a mut dyn Pmc2Bus) -> Self {
        Pmc2Engine { bus }
    }

    /// Reads enabled/T1/T2 through F1 12, F1 13 and F1 14 and validates the
    /// numeric fields. Any invalid or unreadable field fails the whole snapshot.
    pub fn query(&mut self) -> Result<NativeChargeSnapshot, String> {
        let enabled = send_returning(
            self.bus,
            spec::PMC2_CMD_CHARGE_LIMIT,
            spec::PMC2_SUB_READ_ENABLED,
        )?;
        if enabled > spec::PMC2_ENABLED_ON {
            return Err(format!(
                "PMC2 returned invalid enabled state 0x{enabled:02X}; expected 0 or 1"
            ));
        }

        let threshold1 = send_returning(
            self.bus,
            spec::PMC2_CMD_CHARGE_LIMIT,
            spec::PMC2_SUB_READ_T1,
        )?;
        if !(spec::PMC2_VALUE_MIN..=spec::PMC2_VALUE_MAX).contains(&threshold1) {
            return Err(format!(
                "PMC2 returned invalid threshold 1 value {threshold1}; expected 0..=100"
            ));
        }

        let threshold2 = send_returning(
            self.bus,
            spec::PMC2_CMD_CHARGE_LIMIT,
            spec::PMC2_SUB_READ_T2,
        )?;
        if !(spec::PMC2_VALUE_MIN..=spec::PMC2_VALUE_MAX).contains(&threshold2) {
            return Err(format!(
                "PMC2 returned invalid threshold 2 value {threshold2}; expected 0..=100"
            ));
        }

        Ok(NativeChargeSnapshot {
            enabled,
            threshold1,
            threshold2,
        })
    }

    fn set_threshold(&mut self, t1: bool, value: u8) -> Result<NativeChargeSnapshot, String> {
        if !(spec::PMC2_VALUE_MIN..=spec::PMC2_VALUE_MAX).contains(&value) {
            return Err(format!(
                "Refusing to write threshold value {value}; firmware accepts 0..=100"
            ));
        }

        let (command, label) = if t1 {
            (spec::PMC2_CMD_SET_T1, "T1")
        } else {
            (spec::PMC2_CMD_SET_T2, "T2")
        };

        let echo = send_returning(self.bus, command, value)
            .map_err(|error| format!("PMC2 set {label} failed: {error}"))?;
        if echo != value {
            return Err(format!(
                "PMC2 set {label} echo mismatch: sent {value}, received {echo}; value not accepted"
            ));
        }

        let state = self.query()?;
        let actual = if t1 {
            state.threshold1
        } else {
            state.threshold2
        };
        if actual != value {
            return Err(format!(
                "PMC2 set {label} verification failed: wrote {value}, independent readback is {actual}"
            ));
        }
        Ok(state)
    }

    fn enable(&mut self) -> Result<NativeChargeSnapshot, String> {
        send_without_response(self.bus, spec::PMC2_CMD_CHARGE_LIMIT, spec::PMC2_SUB_ENABLE)?;
        let state = self.query()?;
        if state.enabled != spec::PMC2_ENABLED_ON {
            return Err(format!(
                "PMC2 enable did not read back as enabled (readback {}); enable is not assumed to have succeeded",
                state.describe()
            ));
        }
        Ok(state)
    }

    /// Locally verified disable/reset. It erases both thresholds; success
    /// requires an explicit `0/0/0` readback.
    fn disable(&mut self) -> Result<NativeChargeSnapshot, String> {
        send_without_response(
            self.bus,
            spec::PMC2_CMD_CHARGE_LIMIT,
            spec::PMC2_SUB_DISABLE,
        )?;
        let state = self.query()?;
        if !state.is_disabled_erased() {
            return Err(format!(
                "PMC2 disable/reset did not read back as 0/0/0 (observed {}); reset is not confirmed for this call",
                state.describe()
            ));
        }
        Ok(state)
    }
}

// ---------------------------------------------------------------------------------------------
// Transaction / rollback
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
struct AppWrites {
    threshold1: Option<u8>,
    threshold2: Option<u8>,
    enable: bool,
    disable: bool,
}

impl AppWrites {
    fn any(self) -> bool {
        self.threshold1.is_some() || self.threshold2.is_some() || self.enable || self.disable
    }
}

/// Restores only values this call is known to have changed. It may rewrite a
/// pre-existing T2 below 100 when returning the machine to its exact original
/// externally-observed state; that is restoration, not a selected policy target
/// (public applies always target T2=100).
fn rollback(
    engine: &mut Pmc2Engine<'_>,
    original: NativeChargeSnapshot,
    writes: AppWrites,
) -> (String, Option<NativeChargeSnapshot>) {
    if !writes.any() {
        return ("not needed: no app write completed".into(), Some(original));
    }

    let mut notes = Vec::new();
    let mut incomplete = false;
    let mut current = match engine.query() {
        Ok(state) => state,
        Err(error) => {
            return (
                format!(
                    "not attempted: state is unreadable after the failure ({error}); no blind restore write was issued"
                ),
                None,
            )
        }
    };

    let mut cleared_by_disable = false;
    if original.enabled == spec::PMC2_ENABLED_OFF && current.enabled != spec::PMC2_ENABLED_OFF {
        if writes.enable || writes.disable {
            match engine.disable() {
                Ok(state) => {
                    cleared_by_disable = true;
                    current = state;
                    notes.push("disabled the limit this call enabled".to_string());
                }
                Err(error) => {
                    incomplete = true;
                    notes.push(format!("disable during rollback failed: {error}"));
                    if let Ok(state) = engine.query() {
                        current = state;
                        cleared_by_disable = current.is_disabled_erased();
                    }
                }
            }
        } else {
            incomplete = true;
            notes.push(
                "enable state changed outside this call; not disabling it during rollback"
                    .to_string(),
            );
        }
    } else if original.enabled == spec::PMC2_ENABLED_ON && current.enabled != spec::PMC2_ENABLED_ON
    {
        incomplete = true;
        notes.push(
            "enable state changed outside this call; rollback will not re-enable it".to_string(),
        );
    }

    let restore = |engine: &mut Pmc2Engine<'_>,
                   current: &mut NativeChargeSnapshot,
                   t1: bool,
                   original_value: u8,
                   last_written: Option<u8>,
                   cleared_by_disable: bool,
                   notes: &mut Vec<String>,
                   incomplete: &mut bool| {
        let actual = if t1 {
            current.threshold1
        } else {
            current.threshold2
        };
        let label = if t1 { "T1" } else { "T2" };

        if actual == original_value {
            return;
        }

        let caused_by_app = last_written == Some(actual)
            || (cleared_by_disable && actual == spec::PMC2_VALUE_MIN && last_written.is_some());
        if !caused_by_app {
            *incomplete = true;
            notes.push(format!(
                "{label} changed outside this call (observed {actual}, original {original_value}); not overwritten"
            ));
            return;
        }

        match engine.set_threshold(t1, original_value) {
            Ok(state) => {
                *current = state;
                notes.push(format!("restored {label} to {original_value}"));
            }
            Err(error) => {
                *incomplete = true;
                notes.push(format!(
                    "restore {label} to {original_value} failed: {error}"
                ));
            }
        }
    };

    if original.enabled == spec::PMC2_ENABLED_OFF && current.enabled != spec::PMC2_ENABLED_OFF {
        // Writing the original 0/0/0 while the subsystem is still enabled would
        // make T2=0 and put valid SOC above it: the documented active-discharge
        // region. Refuse that and report an incomplete rollback instead.
        incomplete = true;
        notes.push(
            "threshold restore skipped: limit is still enabled, so restoring T1/T2=0 could enter active discharge"
                .to_string(),
        );
    } else {
        restore(
            engine,
            &mut current,
            true,
            original.threshold1,
            writes.threshold1,
            cleared_by_disable,
            &mut notes,
            &mut incomplete,
        );
        restore(
            engine,
            &mut current,
            false,
            original.threshold2,
            writes.threshold2,
            cleared_by_disable,
            &mut notes,
            &mut incomplete,
        );
    }

    let final_state = engine.query().ok();
    let exact = final_state == Some(original);
    if exact {
        notes.push(format!("verified original state {}", original.describe()));
    } else {
        incomplete = true;
        notes.push(match final_state {
            Some(state) => format!(
                "final readback {} does not match original {}",
                state.describe(),
                original.describe()
            ),
            None => "final rollback readback was unavailable".to_string(),
        });
    }

    let prefix = if incomplete { "incomplete" } else { "restored" };
    (format!("{prefix}: {}", notes.join("; ")), final_state)
}

fn failed_outcome(
    engine: &mut Pmc2Engine<'_>,
    original: NativeChargeSnapshot,
    writes: AppWrites,
    failure: String,
) -> ApplyOutcome {
    let (rollback, state) = rollback(engine, original, writes);
    ApplyOutcome {
        applied: false,
        failed: Some(failure),
        rollback: Some(rollback),
        state,
    }
}

/// Pure query entry point used by the Windows wrapper and source tests.
pub fn query_with_bus(gate: &IdentityCheck, bus: &mut dyn Pmc2Bus) -> ChargeState {
    if !gate.matched {
        return ChargeState::from_gate(gate);
    }

    let mut engine = Pmc2Engine::new(bus);
    match engine.query() {
        Ok(snapshot) => ChargeState::from_snapshot(snapshot),
        Err(error) => ChargeState::unavailable(error),
    }
}

/// Pure transactional apply entry point used by the Windows wrapper and source tests.
pub fn apply_with_bus(
    gate: &IdentityCheck,
    bus: &mut dyn Pmc2Bus,
    enabled: bool,
    threshold: u8,
    expected: NativeChargeSnapshot,
) -> Result<ApplyOutcome, String> {
    if !gate.matched {
        return Err(gate
            .detail
            .clone()
            .unwrap_or_else(|| "P916F-STX PMC2 is disabled for this machine".into()));
    }

    let target = validate_target(enabled, threshold)?;
    let mut engine = Pmc2Engine::new(bus);

    let current = engine.query()?;
    if current != expected {
        return Ok(ApplyOutcome {
            applied: false,
            failed: Some(format!(
                "PMC2 state changed since the UI snapshot (expected {}, observed {}); no write was attempted. Re-query before applying",
                expected.describe(),
                current.describe()
            )),
            rollback: Some("not attempted: external state changed; no write was issued".into()),
            state: Some(current),
        });
    }

    if !target.enabled {
        if current.is_disabled_erased() {
            return Ok(ApplyOutcome::success(current));
        }
        let mut writes = AppWrites {
            disable: true,
            ..AppWrites::default()
        };
        return match engine.disable() {
            Ok(state) => Ok(ApplyOutcome::success(state)),
            Err(error) => {
                writes.disable = true;
                Ok(failed_outcome(&mut engine, current, writes, error))
            }
        };
    }

    let target_snapshot = NativeChargeSnapshot {
        enabled: spec::PMC2_ENABLED_ON,
        threshold1: target.threshold1.expect("validated enabled target has T1"),
        threshold2: target.threshold2.expect("validated enabled target has T2"),
    };

    if current == target_snapshot {
        return Ok(ApplyOutcome::success(current));
    }

    let mut writes = AppWrites::default();
    let mut state = current;

    if state.threshold1 != target_snapshot.threshold1 {
        writes.threshold1 = Some(target_snapshot.threshold1);
        match engine.set_threshold(true, target_snapshot.threshold1) {
            Ok(next) => state = next,
            Err(error) => {
                return Ok(failed_outcome(&mut engine, current, writes, error));
            }
        }
    }

    if state.threshold2 != target_snapshot.threshold2 {
        writes.threshold2 = Some(target_snapshot.threshold2);
        match engine.set_threshold(false, target_snapshot.threshold2) {
            Ok(next) => state = next,
            Err(error) => {
                return Ok(failed_outcome(&mut engine, current, writes, error));
            }
        }
    }

    if state.enabled != spec::PMC2_ENABLED_ON {
        writes.enable = true;
        match engine.enable() {
            Ok(_) => {}
            Err(error) => {
                return Ok(failed_outcome(&mut engine, current, writes, error));
            }
        }
    }

    let final_state = match engine.query() {
        Ok(next) => next,
        Err(error) => {
            return Ok(failed_outcome(&mut engine, current, writes, error));
        }
    };

    if final_state != target_snapshot {
        return Ok(failed_outcome(
            &mut engine,
            current,
            writes,
            format!(
                "final readback {} does not match requested {}",
                final_state.describe(),
                target_snapshot.describe()
            ),
        ));
    }

    Ok(ApplyOutcome::success(final_state))
}

// ---------------------------------------------------------------------------------------------
// Windows backend: existing inpoutx64 device only + named mutex
// ---------------------------------------------------------------------------------------------

static PROCESS_PMC2_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[cfg(windows)]
mod windows_backend {
    use super::*;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    // Only the named-mutex calls are declared here because the test crate's
    // `windows` feature set does not include Win32_System_Threading. These are
    // plain kernel32 imports; no extra dependency is added.
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(
            lp_mutex_attributes: *const c_void,
            b_initial_owner: i32,
            lp_name: *const u16,
        ) -> HANDLE;
        fn WaitForSingleObject(handle: HANDLE, milliseconds: u32) -> u32;
        fn ReleaseMutex(mutex: HANDLE) -> i32;
    }

    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_ABANDONED: u32 = 0x80;
    const WAIT_TIMEOUT: u32 = 0x102;
    const WAIT_FAILED: u32 = 0xFFFF_FFFF;

    static ABANDONED_MUTEX_SEEN: AtomicBool = AtomicBool::new(false);

    /// Returns and clears the "previous owner terminated while holding PMC2"
    /// observation. Ownership is still valid when this is true.
    pub fn take_abandoned_mutex_seen() -> bool {
        ABANDONED_MUTEX_SEEN.swap(false, Ordering::SeqCst)
    }

    struct NamedPmc2Mutex {
        handle: HANDLE,
        abandoned: bool,
    }

    impl NamedPmc2Mutex {
        fn acquire(name: &str, timeout_ms: u32) -> Result<Self, String> {
            let mut wide: Vec<u16> = name.encode_utf16().collect();
            wide.push(0);

            let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
            if handle.is_invalid() {
                return Err(format!(
                    "CreateMutexW({name}) failed (Windows error {})",
                    unsafe { GetLastError() }.0
                ));
            }

            match unsafe { WaitForSingleObject(handle, timeout_ms) } {
                WAIT_OBJECT_0 => Ok(NamedPmc2Mutex {
                    handle,
                    abandoned: false,
                }),
                WAIT_ABANDONED => {
                    ABANDONED_MUTEX_SEEN.store(true, Ordering::SeqCst);
                    Ok(NamedPmc2Mutex {
                        handle,
                        abandoned: true,
                    })
                }
                WAIT_TIMEOUT => {
                    unsafe {
                        let _ = CloseHandle(handle);
                    }
                    Err(format!(
                        "PMC2 ownership is busy: another process holds {name} (waited {timeout_ms} ms); no hardware access attempted"
                    ))
                }
                WAIT_FAILED => {
                    let error = unsafe { GetLastError() }.0;
                    unsafe {
                        let _ = CloseHandle(handle);
                    }
                    Err(format!(
                        "WaitForSingleObject({name}) failed (Windows error {error})"
                    ))
                }
                other => {
                    unsafe {
                        let _ = CloseHandle(handle);
                    }
                    Err(format!(
                        "Unexpected WaitForSingleObject({name}) result 0x{other:08X}"
                    ))
                }
            }
        }

        fn abandoned(&self) -> bool {
            self.abandoned
        }
    }

    impl Drop for NamedPmc2Mutex {
        fn drop(&mut self) {
            unsafe {
                let _ = ReleaseMutex(self.handle);
                let _ = CloseHandle(self.handle);
            }
        }
    }

    struct InpoutPmc2Bus {
        handle: HANDLE,
        started: Instant,
    }

    impl InpoutPmc2Bus {
        fn open() -> Result<Self, String> {
            let mut wide: Vec<u16> = spec::DEVICE_PATH.encode_utf16().collect();
            wide.push(0);

            let handle = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            }
            .map_err(|error| {
                format!(
                    "Existing device {} could not be opened: {error}. OpenRevo did not install, start, delete or download any driver",
                    spec::DEVICE_PATH
                )
            })?;

            Ok(InpoutPmc2Bus {
                handle,
                started: Instant::now(),
            })
        }
    }

    impl Drop for InpoutPmc2Bus {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }

    impl Pmc2Bus for InpoutPmc2Bus {
        fn read_port(&mut self, port: u16) -> Result<u8, String> {
            check_port(port)?;
            let input = port.to_le_bytes();
            let mut output = [0u8; 1];
            let mut returned = 0u32;

            unsafe {
                DeviceIoControl(
                    self.handle,
                    spec::IOCTL_READ_PORT_UCHAR,
                    Some(input.as_ptr() as _),
                    input.len() as u32,
                    Some(output.as_mut_ptr() as _),
                    output.len() as u32,
                    Some(&mut returned),
                    None,
                )
            }
            .map_err(|error| format!("inpoutx64 read port 0x{port:02X} failed: {error}"))?;

            if returned != output.len() as u32 {
                return Err(format!(
                    "inpoutx64 read port 0x{port:02X} returned {returned} bytes, expected 1"
                ));
            }
            Ok(output[0])
        }

        fn write_port(&mut self, port: u16, value: u8) -> Result<(), String> {
            check_port(port)?;
            let input = [port as u8, (port >> 8) as u8, value];

            unsafe {
                DeviceIoControl(
                    self.handle,
                    spec::IOCTL_WRITE_PORT_UCHAR,
                    Some(input.as_ptr() as _),
                    input.len() as u32,
                    None,
                    0,
                    None,
                    None,
                )
            }
            .map_err(|error| {
                format!("inpoutx64 write port 0x{port:02X} value 0x{value:02X} failed: {error}")
            })
        }

        fn now_ms(&self) -> u64 {
            self.started.elapsed().as_millis() as u64
        }

        fn sleep_ms(&mut self, ms: u64) {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }

    pub fn with_device<T>(
        operation: impl FnOnce(&mut dyn Pmc2Bus) -> Result<T, String>,
    ) -> Result<T, String> {
        let _process = PROCESS_PMC2_LOCK.lock();
        let global = NamedPmc2Mutex::acquire(spec::PMC2_MUTEX_NAME, spec::PMC2_MUTEX_WAIT_MS)?;
        if global.abandoned() {
            // The prior owner died; the mutex is ours now. Surface the fact via
            // `take_abandoned_mutex_seen` instead of pretending it never happened.
            ABANDONED_MUTEX_SEEN.store(true, Ordering::SeqCst);
        }
        let mut bus = InpoutPmc2Bus::open()?;
        operation(&mut bus)
    }
}

// ---------------------------------------------------------------------------------------------
// Public Windows-facing entry points
// ---------------------------------------------------------------------------------------------

/// Queries the live state through the existing driver. Non-matched machines
/// return `matched=false` without touching the device.
pub fn query_state(gate: &IdentityCheck) -> ChargeState {
    if !gate.matched {
        return ChargeState::from_gate(gate);
    }

    #[cfg(windows)]
    {
        match windows_backend::with_device(|bus| Ok(query_with_bus(gate, bus))) {
            Ok(state) => state,
            Err(error) => ChargeState::unavailable(error),
        }
    }

    #[cfg(not(windows))]
    {
        ChargeState::unavailable("Windows PMC2 backend is unavailable on this OS")
    }
}

/// Applies one policy transaction. Invalid requests and stale expected snapshots
/// fail before any write. The returned state is always a fresh readback.
pub fn apply_policy(
    gate: &IdentityCheck,
    enabled: bool,
    threshold: u8,
    expected: NativeChargeSnapshot,
) -> Result<ApplyOutcome, String> {
    if !gate.matched {
        return Err(gate
            .detail
            .clone()
            .unwrap_or_else(|| "P916F-STX PMC2 is disabled for this machine".into()));
    }

    // Reject unsupported requests before opening the device.
    validate_request(enabled, threshold)?;

    #[cfg(windows)]
    {
        windows_backend::with_device(|bus| apply_with_bus(gate, bus, enabled, threshold, expected))
    }

    #[cfg(not(windows))]
    {
        let _ = expected;
        Err("Windows PMC2 backend is unavailable on this OS".into())
    }
}

/// True if the last completed transaction observed WAIT_ABANDONED on the named
/// mutex. Ownership is valid; callers may log this as a diagnostic.
pub fn take_abandoned_mutex_seen() -> bool {
    #[cfg(windows)]
    {
        windows_backend::take_abandoned_mutex_seen()
    }
    #[cfg(not(windows))]
    {
        false
    }
}
