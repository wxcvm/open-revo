//! Deterministic transaction tests. No Tauri, hardware or OS calls are made:
//! every backend reaction below is scripted.

use openrevo_native_system_tests::usage_txn::{self, Backend, Field, ManualEnergy, Ownership, Plan, Value};
use std::collections::HashMap;

#[derive(Clone)]
enum WriteFault {
    /// Reject the write without touching the value.
    Reject,
    /// Apply the value, then report an error (setter side effect).
    MutateThenFail,
    /// Store a different value and report success.
    WrongValue,
    /// Reject, and let a different field be changed by "something else".
    RejectAfterExternal(Field, Value),
}

#[derive(Default)]
struct Mock {
    values: HashMap<Field, Value>,
    initial: HashMap<Field, Value>,
    faults: HashMap<Field, WriteFault>,
    /// Writes back to the preflight value are rejected (rollback fault).
    reject_restores: HashMap<Field, ()>,
    /// Remaining reads of a field that fail as "readback unavailable".
    unreadable: HashMap<Field, u32>,
    /// Same, but only after this attempt wrote the field (setter verification).
    unreadable_after_write: HashMap<Field, u32>,
    /// Dedicated display rollback state was requested for these fields.
    checkpoints: Vec<Field>,
    /// Dedicated rollback callbacks actually used.
    rollbacks: Vec<Field>,
    /// Model an external display change that dedicated rollback must refuse.
    refuse_refresh_rollback: bool,
    writes: Vec<(Field, Value)>,
}

impl Mock {
    fn new(values: &[(Field, Value)]) -> Self {
        let values: HashMap<Field, Value> = values.iter().cloned().collect();
        Mock { initial: values.clone(), values, ..Mock::default() }
    }
    fn writes_to(&self, field: Field) -> Vec<Value> {
        self.writes.iter().filter(|(f, _)| *f == field).map(|(_, v)| v.clone()).collect()
    }
    fn get(&self, field: Field) -> Value {
        self.values[&field].clone()
    }
}

impl Backend for Mock {
    type Checkpoint = ();
    fn checkpoint(&mut self, field: Field) -> Result<Option<()>, String> {
        if field == Field::RefreshRate {
            self.checkpoints.push(field);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }
    fn rollback(&mut self, field: Field, original: &Value, checkpoint: Option<&()>) -> Result<(), String> {
        if checkpoint.is_some() {
            self.rollbacks.push(field);
            if self.refuse_refresh_rollback {
                return Err("display changed outside this scene; not overwriting it".into());
            }
            self.values.insert(field, original.clone());
            return Ok(());
        }
        self.write(field, original)?;
        let actual = self.read(field)?;
        if actual == *original { Ok(()) } else {
            Err(format!("restore readback mismatch (expected {original}, got {actual})"))
        }
    }
    fn read(&mut self, field: Field) -> Result<Value, String> {
        if let Some(remaining) = self.unreadable.get_mut(&field) {
            if *remaining > 0 {
                *remaining -= 1;
                return Err("readback unavailable".into());
            }
        }
        let written = self.writes.iter().any(|(f, _)| *f == field);
        if let Some(remaining) = self.unreadable_after_write.get_mut(&field) {
            if written && *remaining > 0 {
                *remaining -= 1;
                return Err("readback unavailable".into());
            }
        }
        self.values.get(&field).cloned().ok_or_else(|| "unavailable".into())
    }
    fn write(&mut self, field: Field, value: &Value) -> Result<(), String> {
        self.writes.push((field, value.clone()));
        if self.reject_restores.contains_key(&field) && self.initial.get(&field) == Some(value) {
            return Err("restore rejected".into());
        }
        // Faults are one-shot so a later rollback write can succeed.
        match self.faults.remove(&field) {
            Some(WriteFault::Reject) => Err("rejected".into()),
            Some(WriteFault::MutateThenFail) => {
                self.values.insert(field, value.clone());
                Err("verification failed after mutation".into())
            }
            Some(WriteFault::WrongValue) => {
                let other = match value {
                    Value::Mode(mode) => Value::Mode(mode.wrapping_add(1)),
                    Value::Brightness(level) => Value::Brightness(level.wrapping_sub(1)),
                    Value::Timer(seconds) => Value::Timer(seconds + 1),
                    Value::Refresh(hz) => Value::Refresh(hz + 1),
                    Value::Energy(bytes) => {
                        let mut bytes = *bytes;
                        bytes[0] ^= 0xff;
                        Value::Energy(bytes)
                    }
                };
                self.values.insert(field, other);
                Ok(())
            }
            Some(WriteFault::RejectAfterExternal(field, external)) => {
                self.values.insert(field, external);
                Err("rejected".into())
            }
            None => {
                self.values.insert(field, value.clone());
                Ok(())
            }
        }
    }
}

const ENERGY_A: [u8; 16] = [1; 16];
const ENERGY_B: [u8; 16] = [2; 16];

fn full_plan() -> Plan {
    Plan { firmware_mode: 3, energy: ENERGY_B, brightness: Some(50), keyboard_timeout: Some(600), refresh_hz: Some(120) }
}

fn initial() -> Vec<(Field, Value)> {
    vec![
        (Field::Firmware, Value::Mode(2)),
        (Field::Energy, Value::Energy(ENERGY_A)),
        (Field::Brightness, Value::Brightness(40)),
        (Field::KeyboardTimeout, Value::Timer(300)),
        (Field::RefreshRate, Value::Refresh(60)),
    ]
}

#[test]
fn full_success_applies_every_step_in_order() {
    let mut mock = Mock::new(&initial());
    let outcome = usage_txn::execute(&mut mock, &full_plan()).unwrap();
    assert!(outcome.succeeded());
    assert_eq!(outcome.applied, ["firmware", "windows_energy", "brightness", "keyboard_timeout", "refresh_rate"]);
    assert!(outcome.failed.is_empty() && outcome.restored.is_empty() && !outcome.reverted);
    assert_eq!(mock.writes.len(), 5);
    assert_eq!(mock.get(Field::Firmware), Value::Mode(3));
    assert_eq!(mock.get(Field::Energy), Value::Energy(ENERGY_B));
    assert_eq!(mock.get(Field::RefreshRate), Value::Refresh(120));
}

#[test]
fn early_failure_stops_all_following_steps() {
    let mut mock = Mock::new(&initial());
    mock.faults.insert(Field::Firmware, WriteFault::Reject);
    let outcome = usage_txn::execute(&mut mock, &full_plan()).unwrap();
    assert!(outcome.failed[0].starts_with("firmware: "));
    assert!(outcome.applied.is_empty());
    assert_eq!(mock.writes.to_vec(), vec![(Field::Firmware, Value::Mode(3))]);
    assert!(outcome.reverted, "rejected write left every field at its original value");
    assert_eq!(mock.get(Field::Brightness), Value::Brightness(40));
}

#[test]
fn setter_side_effect_then_error_is_reverted_and_verified() {
    let mut mock = Mock::new(&initial());
    mock.faults.insert(Field::Brightness, WriteFault::MutateThenFail);
    let outcome = usage_txn::execute(&mut mock, &full_plan()).unwrap();
    assert!(outcome.failed.iter().any(|f| f.starts_with("brightness: ")));
    assert_eq!(outcome.restored, ["brightness", "windows_energy", "firmware"]);
    assert_eq!(mock.get(Field::Brightness), Value::Brightness(40));
    assert_eq!(mock.writes_to(Field::Brightness), [Value::Brightness(50), Value::Brightness(40)]);
    assert!(outcome.reverted, "every touched field was recovered and verified");
    assert_eq!(mock.get(Field::Firmware), Value::Mode(2));
    assert_eq!(mock.get(Field::Energy), Value::Energy(ENERGY_A));
}

#[test]
fn unknown_readback_is_reported_and_never_called_restored() {
    let mut mock = Mock::new(&initial());
    // The write itself succeeds; both the success readback and the rollback
    // readback fail, so the engine cannot know which value is on the machine.
    mock.unreadable_after_write.insert(Field::Brightness, 2);
    let outcome = usage_txn::execute(&mut mock, &full_plan()).unwrap();
    assert!(outcome.failed.iter().any(|f| f.starts_with("brightness: ")));
    assert!(outcome.unverified.iter().any(|u| u.starts_with("brightness: ")));
    assert!(!outcome.restored.contains(&"brightness"), "unverified fields must not be reported as restored");
    assert!(!outcome.reverted);
    assert_eq!(mock.writes_to(Field::Brightness), [Value::Brightness(50)], "no blind rollback write");
}

#[test]
fn externally_changed_value_is_relinquished_not_overwritten() {
    let mut mock = Mock::new(&initial());
    mock.faults.insert(Field::Energy, WriteFault::RejectAfterExternal(Field::Firmware, Value::Mode(5)));
    let outcome = usage_txn::execute(&mut mock, &full_plan()).unwrap();
    assert!(outcome.failed.iter().any(|f| f.starts_with("windows_energy: ")));
    assert!(outcome.relinquished.iter().any(|r| r.starts_with("firmware: ")));
    assert!(outcome.restored.is_empty());
    assert!(!outcome.reverted);
    assert_eq!(mock.get(Field::Firmware), Value::Mode(5));
    assert_eq!(mock.writes_to(Field::Firmware), [Value::Mode(3)], "firmware was not written back");
}

#[test]
fn partial_rollback_is_reported_per_field() {
    let mut mock = Mock::new(&initial());
    mock.faults.insert(Field::Energy, WriteFault::MutateThenFail);
    mock.reject_restores.insert(Field::Firmware, ());
    let outcome = usage_txn::execute(&mut mock, &full_plan()).unwrap();
    assert!(outcome.failed.iter().any(|f| f.starts_with("windows_energy: ")));
    assert_eq!(outcome.restored, ["windows_energy"]);
    assert!(outcome.rollback_failed.iter().any(|f| f.starts_with("firmware: ")));
    assert!(!outcome.reverted);
    assert_eq!(mock.get(Field::Firmware), Value::Mode(3));
    assert_eq!(mock.get(Field::Energy), Value::Energy(ENERGY_A));
}

#[test]
fn successful_write_with_unexpected_readback_is_flagged() {
    let mut mock = Mock::new(&initial());
    mock.faults.insert(Field::KeyboardTimeout, WriteFault::WrongValue);
    let outcome = usage_txn::execute(&mut mock, &full_plan()).unwrap();
    assert!(outcome.failed.iter().any(|f| f.starts_with("keyboard_timeout: readback mismatch")));
    // The engine cannot prove the unexpected value belongs to it, so it reports
    // ownership loss instead of overwriting it.
    assert!(outcome.relinquished.iter().any(|r| r.starts_with("keyboard_timeout: ")));
    assert_eq!(outcome.restored, ["brightness", "windows_energy", "firmware"]);
    assert_eq!(mock.get(Field::KeyboardTimeout), Value::Timer(601));
    assert_eq!(mock.get(Field::RefreshRate), Value::Refresh(60), "refresh never ran after the failure");
}

#[test]
fn preflight_read_failure_changes_nothing() {
    let mut mock = Mock::new(&initial());
    mock.unreadable.insert(Field::KeyboardTimeout, 1);
    let error = usage_txn::execute(&mut mock, &full_plan()).unwrap_err();
    assert!(error.contains("keyboard_timeout"), "{error}");
    assert!(mock.writes.is_empty());
}

#[test]
fn manual_energy_success_and_release_rules() {
    let requested = ENERGY_B;
    let before = ENERGY_A;
    assert_eq!(
        usage_txn::classify_manual_energy(0, requested, before, Ok(ENERGY_B)),
        ManualEnergy::Confirmed
    );
    assert!(usage_txn::classify_manual_energy(0, requested, before, Ok(ENERGY_B)).into_result().is_ok());

    // OS changed despite an error code: linkage must be released, error surfaced.
    let changed = usage_txn::classify_manual_energy(5, requested, before, Ok(ENERGY_B));
    assert_eq!(changed, ManualEnergy::ChangedButUnconfirmed);
    assert!(changed.releases_legacy_linkage());
    assert!(changed.into_result().is_err());

    // Provably rejected: nothing changed, linkage stays enabled.
    let rejected = usage_txn::classify_manual_energy(5, requested, before, Ok(ENERGY_A));
    assert_eq!(rejected, ManualEnergy::Rejected(5));
    assert!(!rejected.releases_legacy_linkage());

    // Unknown readback must not silently leave legacy linkage enabled.
    let unknown = usage_txn::classify_manual_energy(0, requested, before, Err("no readback".into()));
    assert!(unknown.releases_legacy_linkage());
    assert!(unknown.into_result().unwrap_err().contains("no readback"));

    // External scheme appeared: never treated as the requested change.
    let conflict = usage_txn::classify_manual_energy(0, requested, before, Ok([9; 16]));
    assert_eq!(conflict, ManualEnergy::Conflict);
    assert!(conflict.releases_legacy_linkage());
    assert!(conflict.into_result().is_err());
}

#[test]
fn refresh_rollback_uses_the_dedicated_checkpoint_not_the_old_hz() {
    let mut mock = Mock::new(&initial());
    mock.faults.insert(Field::RefreshRate, WriteFault::Reject);
    let outcome = usage_txn::execute(&mut mock, &full_plan()).unwrap();
    assert_eq!(mock.checkpoints, [Field::RefreshRate]);
    assert_eq!(mock.rollbacks, [Field::RefreshRate], "dedicated callback, not a scalar Hz write");
    assert!(outcome.restored.contains(&"refresh_rate"));
    assert_eq!(outcome.restored.len(), 5);
    assert!(outcome.reverted);
}

#[test]
fn dedicated_rollback_refuses_to_overwrite_an_external_display_change() {
    let mut mock = Mock::new(&initial());
    mock.faults.insert(Field::RefreshRate, WriteFault::Reject);
    mock.refuse_refresh_rollback = true;
    let outcome = usage_txn::execute(&mut mock, &full_plan()).unwrap();
    assert!(outcome.rollback_failed.iter().any(|e| e.starts_with("refresh_rate: ")));
    assert!(!outcome.reverted);
}

#[test]
fn checkpoint_ownership_distinguishes_unchanged_ours_and_external() {
    assert_eq!(usage_txn::checkpoint_ownership("a", "a", None), Ownership::Unchanged);
    assert_eq!(usage_txn::checkpoint_ownership("b", "a", Some("b")), Ownership::Ours);
    assert_eq!(usage_txn::checkpoint_ownership("c", "a", Some("b")), Ownership::External);
    assert_eq!(usage_txn::checkpoint_ownership("c", "a", None), Ownership::External);
}
