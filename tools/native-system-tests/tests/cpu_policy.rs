//! Deterministic CPU policy transaction tests. The backend is scripted; no
//! Tauri, PowrProf or hardware call is made.

use openrevo_native_system_tests::cpu_policy::{
    self, Backend, Field, Outcome, SideUpdate, SideValues, Snapshot, Source, Update, Value,
};
use std::collections::HashMap;

#[derive(Clone, Copy)]
enum Fault {
    Reject,
    MutateThenFail,
}

struct Mock {
    plan: String,
    values: HashMap<Field, Option<Value>>,
    write_faults: HashMap<Field, Fault>,
    plan_after_write: HashMap<Field, String>,
    /// Reads of a field fail only after this attempt wrote the field.
    read_failures_after_write: HashMap<Field, u32>,
    overlay: Option<[u8; 16]>,
    overlay_changed_on_activate: Option<Option<[u8; 16]>>,
    read_overlay_failures: u32,
    activate_failures: u32,
    plan_on_activate: Option<String>,
    writes: Vec<(Field, Value)>,
    active_plan_calls: usize,
    activate_calls: usize,
    restore_overlay_calls: usize,
}

impl Mock {
    fn new(snapshot: &Snapshot) -> Self {
        let mut values = HashMap::new();
        for source in [Source::Ac, Source::Dc] {
            let side = snapshot.side(source);
            values.insert(
                Field::MaxProcessorState(source),
                Some(Value::Percent(side.max_processor_state)),
            );
            values.insert(
                Field::BoostMode(source),
                Some(Value::Boost(side.boost_mode)),
            );
            values.insert(Field::MaxFrequencyMhz(source), side.max_mhz.map(Value::Mhz));
        }
        Mock {
            plan: snapshot.plan_guid.clone(),
            values,
            write_faults: HashMap::new(),
            plan_after_write: HashMap::new(),
            read_failures_after_write: HashMap::new(),
            overlay: Some([0x11; 16]),
            overlay_changed_on_activate: None,
            read_overlay_failures: 0,
            activate_failures: 0,
            plan_on_activate: None,
            writes: Vec::new(),
            active_plan_calls: 0,
            activate_calls: 0,
            restore_overlay_calls: 0,
        }
    }

    fn current(&self, field: Field) -> Value {
        self.values.get(&field).copied().flatten().expect("value")
    }
}

impl Backend for Mock {
    fn active_plan(&mut self) -> Result<String, String> {
        self.active_plan_calls += 1;
        Ok(self.plan.clone())
    }

    fn read(&mut self, _plan: &str, field: Field) -> Result<Option<Value>, String> {
        let written = self.writes.iter().any(|(written, _)| *written == field);
        if written {
            if let Some(remaining) = self.read_failures_after_write.get_mut(&field) {
                if *remaining > 0 {
                    *remaining -= 1;
                    return Err("readback unavailable".into());
                }
            }
        }
        Ok(self.values.get(&field).copied().flatten())
    }

    fn write(&mut self, _plan: &str, field: Field, value: &Value) -> Result<(), String> {
        self.writes.push((field, *value));
        let fault = self.write_faults.remove(&field);
        match fault {
            Some(Fault::Reject) => return Err("rejected by policy".into()),
            Some(Fault::MutateThenFail) => {
                self.values.insert(field, Some(*value));
                return Err("setter reported failure after mutation".into());
            }
            None => {
                self.values.insert(field, Some(*value));
            }
        }
        if let Some(plan) = self.plan_after_write.remove(&field) {
            self.plan = plan;
        }
        Ok(())
    }

    fn activate(&mut self, _plan: &str) -> Result<(), String> {
        self.activate_calls += 1;
        if self.activate_failures > 0 {
            self.activate_failures -= 1;
            return Err("activation rejected".into());
        }
        if let Some(plan) = self.plan_on_activate.take() {
            self.plan = plan;
        }
        if let Some(overlay) = self.overlay_changed_on_activate.take() {
            self.overlay = overlay;
        }
        Ok(())
    }

    fn read_overlay(&mut self) -> Result<Option<[u8; 16]>, String> {
        if self.read_overlay_failures > 0 {
            self.read_overlay_failures -= 1;
            return Err("overlay readback unavailable".into());
        }
        Ok(self.overlay)
    }

    fn restore_overlay(&mut self, overlay: &[u8; 16]) -> Result<(), String> {
        self.restore_overlay_calls += 1;
        self.overlay = Some(*overlay);
        Ok(())
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        plan_guid: "381b4222-f694-41f0-9685-ff5bb260df2e".into(),
        ac: SideValues {
            max_processor_state: 100,
            boost_mode: 2,
            max_mhz: Some(0),
        },
        dc: SideValues {
            max_processor_state: 100,
            boost_mode: 2,
            max_mhz: Some(0),
        },
    }
}

fn update(ac_state: Option<u32>, dc_state: Option<u32>) -> Update {
    Update {
        ac: SideUpdate {
            max_processor_state: ac_state,
            ..SideUpdate::default()
        },
        dc: SideUpdate {
            max_processor_state: dc_state,
            ..SideUpdate::default()
        },
    }
}

fn failed_text(outcome: &Outcome) -> String {
    outcome.failed.join(" | ")
}

#[test]
fn partial_failure_rolls_back_only_values_this_attempt_wrote() {
    let mut mock = Mock::new(&snapshot());
    mock.write_faults
        .insert(Field::MaxProcessorState(Source::Dc), Fault::Reject);

    let outcome = cpu_policy::execute(&mut mock, &snapshot(), &update(Some(80), Some(70))).unwrap();

    assert_eq!(outcome.applied, vec!["ac.max_processor_state"]);
    assert!(failed_text(&outcome).contains("dc.max_processor_state"));
    assert_eq!(outcome.restored, vec!["ac.max_processor_state"]);
    assert!(outcome.reverted, "{outcome:?}");
    assert!(
        !outcome.activated,
        "a failed write must never be reported effective"
    );
    assert_eq!(mock.activate_calls, 0);
    assert_eq!(
        mock.current(Field::MaxProcessorState(Source::Ac)),
        Value::Percent(100)
    );
    assert_eq!(
        mock.current(Field::MaxProcessorState(Source::Dc)),
        Value::Percent(100)
    );
    assert_eq!(
        mock.writes,
        vec![
            (Field::MaxProcessorState(Source::Ac), Value::Percent(80)),
            (Field::MaxProcessorState(Source::Dc), Value::Percent(70)),
            (Field::MaxProcessorState(Source::Ac), Value::Percent(100)),
        ]
    );
}

#[test]
fn external_plan_change_blocks_the_next_write_and_cross_plan_rollback() {
    let mut mock = Mock::new(&snapshot());
    mock.plan_after_write.insert(
        Field::MaxProcessorState(Source::Ac),
        "a1841308-3541-4fab-bc81-f71556f20b4a".into(),
    );

    let outcome = cpu_policy::execute(&mut mock, &snapshot(), &update(Some(80), Some(70))).unwrap();

    assert_eq!(outcome.applied, vec!["ac.max_processor_state"]);
    assert!(
        failed_text(&outcome).contains("different plan"),
        "{outcome:?}"
    );
    assert!(outcome
        .rollback_failed
        .iter()
        .any(|item| item.contains("active Windows power plan changed")));
    assert!(outcome.restored.is_empty());
    assert!(!outcome.reverted);
    assert!(!outcome.activated);
    assert_eq!(mock.activate_calls, 0);
    assert_eq!(
        mock.writes,
        vec![(Field::MaxProcessorState(Source::Ac), Value::Percent(80))],
        "the DC value must not be written after the plan identity changed"
    );
}

#[test]
fn plan_changed_before_apply_is_rejected_without_any_write() {
    let mut mock = Mock::new(&snapshot());
    mock.plan = "a1841308-3541-4fab-bc81-f71556f20b4a".into();

    let error = cpu_policy::execute(&mut mock, &snapshot(), &update(Some(80), None)).unwrap_err();

    assert!(error.contains("changed from"), "{error}");
    assert!(mock.writes.is_empty());
}

#[test]
fn unknown_readback_is_unverified_and_never_called_restored() {
    let mut mock = Mock::new(&snapshot());
    mock.read_failures_after_write
        .insert(Field::MaxProcessorState(Source::Ac), 2);

    let mut request = Update::default();
    request.ac.max_processor_state = Some(80);
    let outcome = cpu_policy::execute(&mut mock, &snapshot(), &request).unwrap();

    assert!(outcome.applied.is_empty());
    assert!(outcome.restored.is_empty());
    assert_eq!(outcome.unverified.len(), 1, "{outcome:?}");
    assert!(outcome.unverified[0].contains("readback failed"));
    assert!(outcome.rollback_failed.is_empty());
    assert!(!outcome.reverted);
    assert!(!outcome.activated);
    assert_eq!(
        mock.current(Field::MaxProcessorState(Source::Ac)),
        Value::Percent(80)
    );
}

#[test]
fn setter_mutation_after_error_still_gets_conditional_rollback() {
    let mut mock = Mock::new(&snapshot());
    mock.write_faults
        .insert(Field::BoostMode(Source::Ac), Fault::MutateThenFail);
    let mut request = Update::default();
    request.ac.boost_mode = Some(0);

    let outcome = cpu_policy::execute(&mut mock, &snapshot(), &request).unwrap();

    assert!(outcome.applied.is_empty());
    assert!(failed_text(&outcome).contains("boost_mode"));
    assert_eq!(outcome.restored, vec!["ac.boost_mode"]);
    assert!(outcome.reverted);
    assert!(!outcome.activated);
    assert_eq!(mock.activate_calls, 0);
    assert_eq!(mock.current(Field::BoostMode(Source::Ac)), Value::Boost(2));
}

#[test]
fn validation_and_unsupported_mhz_fail_before_any_write() {
    let mut mock = Mock::new(&snapshot());
    let mut request = Update::default();
    request.ac.max_processor_state = Some(4);
    assert!(cpu_policy::execute(&mut mock, &snapshot(), &request).is_err());

    let mut no_mhz = snapshot();
    no_mhz.ac.max_mhz = None;
    no_mhz.dc.max_mhz = None;
    let mut mock = Mock::new(&no_mhz);
    let mut request = Update::default();
    request.ac.max_mhz = Some(4000);
    let error = cpu_policy::execute(&mut mock, &no_mhz, &request).unwrap_err();
    assert!(error.contains("not exposed"), "{error}");

    assert!(mock.writes.is_empty());
    assert_eq!(mock.active_plan_calls, 0);
}

#[test]
fn exact_success_reports_only_readback_verified_fields() {
    let mut mock = Mock::new(&snapshot());
    let mut request = Update::default();
    request.ac.max_processor_state = Some(90);
    request.dc.max_processor_state = Some(100);
    request.dc.boost_mode = Some(1);

    let outcome = cpu_policy::execute(&mut mock, &snapshot(), &request).unwrap();

    assert_eq!(
        outcome.applied,
        vec!["ac.max_processor_state", "dc.boost_mode"]
    );
    assert_eq!(outcome.unchanged, vec!["dc.max_processor_state"]);
    assert!(outcome.failed.is_empty());
    assert!(outcome.restored.is_empty());
    assert!(outcome.succeeded());
    assert!(outcome.activated);
    assert_eq!(mock.activate_calls, 1);
    assert!(outcome.overlay_preserved, "{outcome:?}");
    assert_eq!(mock.current(Field::BoostMode(Source::Dc)), Value::Boost(1));
}

#[test]
fn activation_failure_rolls_back_saved_values_and_reactivates_the_same_plan() {
    let mut mock = Mock::new(&snapshot());
    mock.activate_failures = 1;

    let outcome = cpu_policy::execute(&mut mock, &snapshot(), &update(Some(80), Some(70))).unwrap();

    assert!(outcome.activation_error.is_some(), "{outcome:?}");
    assert!(!outcome.activated);
    assert!(!outcome.succeeded());
    assert_eq!(
        outcome.restored,
        vec!["dc.max_processor_state", "ac.max_processor_state"]
    );
    assert!(outcome.reverted, "{outcome:?}");
    assert!(outcome.reactivated, "{outcome:?}");
    assert_eq!(
        mock.activate_calls, 2,
        "one failed activation, one restore activation"
    );
    assert_eq!(
        mock.current(Field::MaxProcessorState(Source::Ac)),
        Value::Percent(100)
    );
    assert_eq!(
        mock.current(Field::MaxProcessorState(Source::Dc)),
        Value::Percent(100)
    );
}

#[test]
fn failed_reactivation_is_reported_not_hidden() {
    let mut mock = Mock::new(&snapshot());
    mock.activate_failures = 2;

    let outcome = cpu_policy::execute(&mut mock, &snapshot(), &update(Some(80), None)).unwrap();

    assert!(!outcome.activated);
    assert!(!outcome.reactivated);
    assert!(!outcome.reverted);
    assert!(outcome
        .rollback_failed
        .iter()
        .any(|item| item.contains("reactivation")));
    assert_eq!(mock.activate_calls, 2);
}

#[test]
fn external_plan_change_before_activation_never_activates_another_plan() {
    let mut mock = Mock::new(&snapshot());
    mock.plan_after_write.insert(
        Field::MaxProcessorState(Source::Ac),
        "a1841308-3541-4fab-bc81-f71556f20b4a".into(),
    );

    let outcome = cpu_policy::execute(&mut mock, &snapshot(), &update(Some(80), None)).unwrap();

    assert_eq!(outcome.applied, vec!["ac.max_processor_state"]);
    assert!(!outcome.activated);
    assert!(outcome
        .activation_error
        .as_deref()
        .unwrap_or_default()
        .contains("before activation"));
    assert_eq!(mock.activate_calls, 0, "must not switch plans");
    assert!(outcome.restored.is_empty());
}

#[test]
fn activation_observed_overlay_change_is_restored_from_exact_prior_value() {
    let mut mock = Mock::new(&snapshot());
    mock.overlay = Some([0x11; 16]);
    mock.overlay_changed_on_activate = Some(Some([0x22; 16]));

    let outcome = cpu_policy::execute(&mut mock, &snapshot(), &update(Some(80), None)).unwrap();

    assert!(outcome.activated);
    assert_eq!(
        outcome.overlay_before.as_deref(),
        Some("11111111111111111111111111111111")
    );
    assert_eq!(
        outcome.overlay_after.as_deref(),
        Some("22222222222222222222222222222222")
    );
    assert!(outcome.overlay_preserved, "{outcome:?}");
    assert!(outcome.overlay_error.is_none(), "{outcome:?}");
    assert_eq!(mock.restore_overlay_calls, 1);
    assert_eq!(mock.overlay, Some([0x11; 16]));
}

#[test]
fn unknown_overlay_is_never_overwritten() {
    let mut mock = Mock::new(&snapshot());
    mock.overlay = None;

    let outcome = cpu_policy::execute(&mut mock, &snapshot(), &update(Some(80), None)).unwrap();

    assert!(
        outcome.activated,
        "CPU policy is effective even if overlay is unknown"
    );
    assert!(outcome.overlay_before.is_none());
    assert!(outcome.overlay_after.is_none());
    assert!(!outcome.overlay_preserved);
    assert!(outcome.overlay_error.is_some());
    assert_eq!(mock.restore_overlay_calls, 0);
}

#[test]
fn restore_update_covers_exact_snapshot_values_and_exposed_mhz() {
    let snapshot = snapshot();
    let update = cpu_policy::restore_update(&snapshot);
    let steps = update.steps(&snapshot).unwrap();
    assert_eq!(
        steps,
        vec![
            (Field::MaxProcessorState(Source::Ac), Value::Percent(100)),
            (Field::BoostMode(Source::Ac), Value::Boost(2)),
            (Field::MaxFrequencyMhz(Source::Ac), Value::Mhz(0)),
            (Field::MaxProcessorState(Source::Dc), Value::Percent(100)),
            (Field::BoostMode(Source::Dc), Value::Boost(2)),
            (Field::MaxFrequencyMhz(Source::Dc), Value::Mhz(0)),
        ]
    );
}
