//! Deterministic P916F-STX PMC2 charge-limit tests.
//!
//! The pure tests below use a scripted PMC2 port model and never open a device.
//! Production-gated live acceptance lives in commands::charge_cmd tests.

use openrevo_native_system_tests::p916f_charge::{
    self, ChargeState, IdentityCheck, NativeChargeSnapshot, Pmc2Bus,
};
use openrevo_native_system_tests::vault::p916f_pmc2 as spec;

// ---------------------------------------------------------------------------------------------
// Scripted PMC2 controller
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// Reject the write without changing the field.
    ErrorNoMutate,
    /// Change the field, then report a driver/write error.
    MutateThenError,
    /// Change the field and return a mismatching echo byte.
    WrongEcho,
}

struct MockBus {
    state: NativeChargeSnapshot,
    writes: Vec<(u16, u8)>,
    threshold_writes: Vec<(u8, u8)>,
    command: Option<u8>,
    output: Option<u8>,
    obf: bool,
    stuck_ibf: bool,
    no_obf: bool,
    absent: bool,
    now_ms: u64,
    step_ms: u64,
    enabled_override: Option<u8>,
    t1_override: Option<u8>,
    t2_override: Option<u8>,
    t1_fault: Fault,
    t2_fault: Fault,
    enable_noop: bool,
    ignore_disable: bool,
    enable_seen: bool,
    last_subcommand: Option<u8>,
    corrupt_t2_query_once_after_enable: bool,
    corrupt_t2_query_injected: bool,
}

impl MockBus {
    fn new(enabled: u8, threshold1: u8, threshold2: u8) -> Self {
        MockBus {
            state: NativeChargeSnapshot {
                enabled,
                threshold1,
                threshold2,
            },
            writes: Vec::new(),
            threshold_writes: Vec::new(),
            command: None,
            output: None,
            obf: false,
            stuck_ibf: false,
            no_obf: false,
            absent: false,
            now_ms: 0,
            step_ms: 10,
            enabled_override: None,
            t1_override: None,
            t2_override: None,
            t1_fault: Fault::None,
            t2_fault: Fault::None,
            enable_noop: false,
            ignore_disable: false,
            enable_seen: false,
            last_subcommand: None,
            corrupt_t2_query_once_after_enable: false,
            corrupt_t2_query_injected: false,
        }
    }

    fn setter_commands(&self) -> Vec<u8> {
        self.writes
            .iter()
            .filter(|(port, value)| {
                *port == spec::PMC2_COMMAND_PORT
                    && (*value == spec::PMC2_CMD_SET_T1 || *value == spec::PMC2_CMD_SET_T2)
            })
            .map(|(_, value)| *value)
            .collect()
    }

    fn threshold_values_for(&self, command: u8) -> Vec<u8> {
        self.threshold_writes
            .iter()
            .filter(|(kind, _)| *kind == command)
            .map(|(_, value)| *value)
            .collect()
    }

    fn respond(&mut self, value: u8) {
        self.output = Some(value);
        if !self.no_obf {
            self.obf = true;
        }
    }
}

impl Pmc2Bus for MockBus {
    fn read_port(&mut self, port: u16) -> Result<u8, String> {
        if port != spec::PMC2_DATA_PORT && port != spec::PMC2_COMMAND_PORT {
            return Err("mock received a non-allowlisted port".into());
        }
        if self.absent {
            return Ok(spec::PMC2_ABSENT);
        }

        if port == spec::PMC2_COMMAND_PORT {
            let mut status = 0u8;
            if self.obf {
                status |= spec::PMC2_STATUS_OBF;
            }
            if self.stuck_ibf {
                status |= spec::PMC2_STATUS_IBF;
            }
            return Ok(status);
        }

        if !self.obf {
            return Ok(spec::PMC2_ABSENT);
        }
        self.obf = false;
        let value = self.output.take().unwrap_or(spec::PMC2_ABSENT);
        if self.corrupt_t2_query_once_after_enable
            && self.enable_seen
            && self.last_subcommand == Some(spec::PMC2_SUB_READ_T2)
            && !self.corrupt_t2_query_injected
        {
            self.corrupt_t2_query_injected = true;
            return Ok(spec::PMC2_ABSENT);
        }
        Ok(value)
    }

    fn write_port(&mut self, port: u16, value: u8) -> Result<(), String> {
        if port != spec::PMC2_DATA_PORT && port != spec::PMC2_COMMAND_PORT {
            return Err("mock received a non-allowlisted port".into());
        }
        self.writes.push((port, value));

        if port == spec::PMC2_COMMAND_PORT {
            self.command = Some(value);
            return Ok(());
        }

        let command = self.command.take().ok_or("data write without command")?;
        match command {
            spec::PMC2_CMD_CHARGE_LIMIT => {
                self.last_subcommand = Some(value);
                let response = match value {
                    spec::PMC2_SUB_READ_ENABLED => {
                        Some(self.enabled_override.unwrap_or(self.state.enabled))
                    }
                    spec::PMC2_SUB_READ_T1 => {
                        Some(self.t1_override.unwrap_or(self.state.threshold1))
                    }
                    spec::PMC2_SUB_READ_T2 => {
                        Some(self.t2_override.unwrap_or(self.state.threshold2))
                    }
                    spec::PMC2_SUB_ENABLE => {
                        self.enable_seen = true;
                        if !self.enable_noop {
                            self.state.enabled = spec::PMC2_ENABLED_ON;
                        }
                        None
                    }
                    spec::PMC2_SUB_DISABLE => {
                        if !self.ignore_disable {
                            self.state = NativeChargeSnapshot {
                                enabled: spec::PMC2_ENABLED_OFF,
                                threshold1: 0,
                                threshold2: 0,
                            };
                        }
                        None
                    }
                    _ => return Err(format!("unexpected charge-limit subcommand 0x{value:02X}")),
                };

                if let Some(response) = response {
                    self.respond(response);
                }
                Ok(())
            }
            spec::PMC2_CMD_SET_T1 | spec::PMC2_CMD_SET_T2 => {
                self.threshold_writes.push((command, value));
                let fault = if command == spec::PMC2_CMD_SET_T1 {
                    let fault = self.t1_fault;
                    self.t1_fault = Fault::None;
                    fault
                } else {
                    let fault = self.t2_fault;
                    self.t2_fault = Fault::None;
                    fault
                };

                match fault {
                    Fault::None => {
                        if command == spec::PMC2_CMD_SET_T1 {
                            self.state.threshold1 = value;
                        } else {
                            self.state.threshold2 = value;
                        }
                        self.respond(value);
                        Ok(())
                    }
                    Fault::ErrorNoMutate => Err("injected setter error before mutation".into()),
                    Fault::MutateThenError => {
                        if command == spec::PMC2_CMD_SET_T1 {
                            self.state.threshold1 = value;
                        } else {
                            self.state.threshold2 = value;
                        }
                        Err("injected setter error after mutation".into())
                    }
                    Fault::WrongEcho => {
                        if command == spec::PMC2_CMD_SET_T1 {
                            self.state.threshold1 = value;
                        } else {
                            self.state.threshold2 = value;
                        }
                        self.respond(value.wrapping_add(1));
                        Ok(())
                    }
                }
            }
            _ => Err(format!("unexpected command 0x{command:02X}")),
        }
    }

    fn now_ms(&self) -> u64 {
        self.now_ms
    }

    fn sleep_ms(&mut self, ms: u64) {
        self.now_ms = self
            .now_ms
            .saturating_add(if self.step_ms == 0 { ms } else { self.step_ms });
    }
}

fn confirmed() -> IdentityCheck {
    p916f_charge::verify_identity(Some(spec::MODEL_ID), spec::BIOS_VERSION)
}

fn snap(enabled: u8, threshold1: u8, threshold2: u8) -> NativeChargeSnapshot {
    NativeChargeSnapshot {
        enabled,
        threshold1,
        threshold2,
    }
}

fn expect_state(state: ChargeState) -> NativeChargeSnapshot {
    assert!(state.matched, "expected matched machine: {state:?}");
    assert!(state.available, "expected available state: {state:?}");
    snap(
        state.enabled.expect("enabled"),
        state.threshold1.expect("threshold1"),
        state.threshold2.expect("threshold2"),
    )
}

// ---------------------------------------------------------------------------------------------
// Identity / unsupported / no-touch guarantees
// ---------------------------------------------------------------------------------------------

#[test]
fn identity_requires_exact_model_and_bios() {
    assert!(p916f_charge::verify_identity(Some(spec::MODEL_ID), "1.15").is_matched());
    assert!(p916f_charge::verify_identity(Some(spec::MODEL_ID), " 1.15 ").is_matched());

    let no_manifest = p916f_charge::verify_identity(None, "1.15");
    assert!(!no_manifest.is_matched());

    let other_model = p916f_charge::verify_identity(Some("mechrevo_xingyao_14_p916f"), "1.15");
    assert!(!other_model.is_matched());

    let wrong_bios = p916f_charge::verify_identity(Some(spec::MODEL_ID), "1.14");
    assert!(!wrong_bios.is_matched());
}

#[test]
fn unmatched_machine_never_touches_device() {
    let gate = p916f_charge::verify_identity(Some("some_other_model"), "1.15");
    let mut bus = MockBus::new(1, 80, 100);

    let state = p916f_charge::query_with_bus(&gate, &mut bus);
    assert!(!state.matched);
    assert!(!state.available);
    assert_eq!(state.enabled, None);
    assert!(bus.writes.is_empty(), "unmatched query wrote to PMC2");

    let error = p916f_charge::apply_with_bus(&gate, &mut bus, true, 80, snap(1, 80, 100))
        .expect_err("unmatched apply must fail");
    assert!(error.contains("disabled") || error.contains("not"));
    assert!(bus.writes.is_empty(), "unmatched apply wrote to PMC2");
}

#[test]
fn unsupported_policy_is_rejected_before_any_io() {
    assert!(p916f_charge::validate_request(true, 100).is_err());
    assert!(p916f_charge::validate_request(true, 0).is_err());
    assert!(p916f_charge::validate_request(true, 101).is_err());
    assert!(p916f_charge::validate_request(false, 80).is_err());
    assert!(p916f_charge::validate_request(true, 1).is_ok());
    assert!(p916f_charge::validate_request(true, 99).is_ok());
    assert!(p916f_charge::validate_request(false, 100).is_ok());

    let mut bus = MockBus::new(0, 0, 0);
    let error = p916f_charge::apply_with_bus(&confirmed(), &mut bus, true, 100, snap(0, 0, 0))
        .expect_err("T1=100 must be rejected");
    assert!(error.contains("T1==T2") || error.contains("disable"));
    assert!(bus.writes.is_empty(), "invalid request reached PMC2");
}

// ---------------------------------------------------------------------------------------------
// Query validation: absent, stale, timeout, invalid values
// ---------------------------------------------------------------------------------------------

#[test]
fn query_reads_all_three_fields() {
    let mut bus = MockBus::new(1, 80, 100);
    let state = p916f_charge::query_with_bus(&confirmed(), &mut bus);
    assert_eq!(expect_state(state), snap(1, 80, 100));
    assert_eq!(bus.setter_commands(), Vec::<u8>::new());
}

#[test]
fn absent_controller_reads_are_rejected() {
    let mut bus = MockBus::new(0, 0, 0);
    bus.absent = true;
    let state = p916f_charge::query_with_bus(&confirmed(), &mut bus);
    assert!(!state.available);
    assert!(state.reason.as_deref().unwrap_or("").contains("0xFF"));
    assert!(state.enabled.is_none());
}

#[test]
fn stale_obf_is_refused_without_draining_or_writing() {
    let mut bus = MockBus::new(1, 80, 100);
    bus.obf = true;
    bus.output = Some(0x42);

    let state = p916f_charge::query_with_bus(&confirmed(), &mut bus);
    assert!(!state.available);
    assert!(state
        .reason
        .as_deref()
        .unwrap_or("")
        .contains("stale output"));
    assert!(bus.writes.is_empty(), "stale OBF path issued a command");
    assert_eq!(bus.output, Some(0x42), "stale byte was consumed");
}

#[test]
fn ibf_timeout_is_bounded() {
    let mut bus = MockBus::new(1, 80, 100);
    bus.stuck_ibf = true;

    let state = p916f_charge::query_with_bus(&confirmed(), &mut bus);
    assert!(!state.available);
    assert!(
        state.reason.as_deref().unwrap_or("").contains("timed out"),
        "unexpected reason: {state:?}"
    );
    assert!(bus.now_ms >= spec::PMC2_WAIT_TIMEOUT_MS);
    assert!(
        bus.writes.is_empty(),
        "timed-out preflight issued a command"
    );
}

#[test]
fn obf_timeout_is_bounded() {
    let mut bus = MockBus::new(1, 80, 100);
    bus.no_obf = true;

    let state = p916f_charge::query_with_bus(&confirmed(), &mut bus);
    assert!(!state.available);
    assert!(state.reason.as_deref().unwrap_or("").contains("timed out"));
    assert!(!bus.writes.is_empty());

    let mut bus = MockBus::new(0, 0, 0);
    bus.no_obf = true;
    let error = p916f_charge::apply_with_bus(&confirmed(), &mut bus, false, 100, snap(0, 0, 0))
        .expect_err("expected snapshot query to time out");
    assert!(error.contains("timed out"));
}

#[test]
fn invalid_values_are_rejected_not_displayed() {
    for (enabled, t1, t2) in [(2, 80, 100), (1, 101, 100), (1, 80, 101)] {
        let mut bus = MockBus::new(0, 0, 0);
        bus.enabled_override = Some(enabled);
        bus.t1_override = Some(t1);
        bus.t2_override = Some(t2);

        let state = p916f_charge::query_with_bus(&confirmed(), &mut bus);
        assert!(!state.available, "invalid {enabled}/{t1}/{t2} was accepted");
        assert!(state.reason.is_some());
        assert!(state.enabled.is_none() && state.threshold1.is_none());
    }

    let mut bus = MockBus::new(0, 0, 0);
    bus.t1_override = Some(spec::PMC2_ABSENT);
    let state = p916f_charge::query_with_bus(&confirmed(), &mut bus);
    assert!(!state.available);
    assert!(state.reason.as_deref().unwrap_or("").contains("0xFF"));
}

// ---------------------------------------------------------------------------------------------
// Transaction: stale expected snapshot and partial-failure rollback
// ---------------------------------------------------------------------------------------------

#[test]
fn changed_external_state_is_refused_before_setters() {
    let mut bus = MockBus::new(0, 80, 100);
    let outcome = p916f_charge::apply_with_bus(&confirmed(), &mut bus, true, 60, snap(0, 0, 0))
        .expect("transaction report");
    assert!(!outcome.applied);
    assert!(outcome.failed.as_deref().unwrap_or("").contains("changed"));
    assert_eq!(outcome.state, Some(snap(0, 80, 100)));
    assert!(outcome
        .rollback
        .as_deref()
        .unwrap_or("")
        .contains("not attempted"));
    assert!(
        bus.setter_commands().is_empty(),
        "setter issued after external state change"
    );
    assert_eq!(bus.state, snap(0, 80, 100));
}

#[test]
fn partial_t2_failure_restores_the_t1_write() {
    let mut bus = MockBus::new(0, 0, 0);
    bus.t2_fault = Fault::MutateThenError;

    let outcome = p916f_charge::apply_with_bus(&confirmed(), &mut bus, true, 80, snap(0, 0, 0))
        .expect("transaction report");
    assert!(!outcome.applied);
    assert!(outcome.failed.as_deref().unwrap_or("").contains("T2"));
    assert!(
        outcome
            .rollback
            .as_deref()
            .unwrap_or("")
            .starts_with("restored"),
        "rollback did not restore: {:?}",
        outcome.rollback
    );
    assert_eq!(outcome.state, Some(snap(0, 0, 0)));
    assert_eq!(bus.state, snap(0, 0, 0));
}

#[test]
fn first_setter_mutated_after_error_is_detected_by_readback_and_restored() {
    let mut bus = MockBus::new(0, 0, 0);
    bus.t1_fault = Fault::MutateThenError;

    let outcome = p916f_charge::apply_with_bus(&confirmed(), &mut bus, true, 80, snap(0, 0, 0))
        .expect("transaction report");
    assert!(!outcome.applied);
    assert!(
        outcome
            .failed
            .as_deref()
            .unwrap_or("")
            .contains("before mutation")
            || outcome
                .failed
                .as_deref()
                .unwrap_or("")
                .contains("after mutation")
    );
    assert!(
        outcome
            .rollback
            .as_deref()
            .unwrap_or("")
            .starts_with("restored"),
        "readback did not detect/restore mutation: {:?}",
        outcome.rollback
    );
    assert_eq!(outcome.state, Some(snap(0, 0, 0)));
    assert_eq!(bus.state, snap(0, 0, 0));
    assert_eq!(
        bus.threshold_values_for(spec::PMC2_CMD_SET_T2),
        Vec::<u8>::new()
    );
}

#[test]
fn setter_error_before_mutation_needs_no_restore_write() {
    let mut bus = MockBus::new(0, 0, 0);
    bus.t1_fault = Fault::ErrorNoMutate;

    let outcome = p916f_charge::apply_with_bus(&confirmed(), &mut bus, true, 80, snap(0, 0, 0))
        .expect("transaction report");
    assert!(!outcome.applied);
    assert!(outcome.failed.as_deref().unwrap_or("").contains("T1"));
    assert_eq!(outcome.state, Some(snap(0, 0, 0)));
    assert_eq!(bus.state, snap(0, 0, 0));
    assert_eq!(
        bus.threshold_values_for(spec::PMC2_CMD_SET_T1),
        vec![80],
        "only the rejected attempt should have reached the setter"
    );
}

#[test]
fn echo_mismatch_is_not_success_and_rolls_back() {
    let mut bus = MockBus::new(0, 0, 0);
    bus.t1_fault = Fault::WrongEcho;

    let outcome = p916f_charge::apply_with_bus(&confirmed(), &mut bus, true, 80, snap(0, 0, 0))
        .expect("transaction report");
    assert!(!outcome.applied);
    assert!(outcome.failed.as_deref().unwrap_or("").contains("echo"));
    assert_eq!(bus.state, snap(0, 0, 0));
}

#[test]
fn enable_readback_failure_rolls_back_thresholds() {
    let mut bus = MockBus::new(0, 0, 0);
    bus.enable_noop = true;

    let outcome = p916f_charge::apply_with_bus(&confirmed(), &mut bus, true, 80, snap(0, 0, 0))
        .expect("transaction report");
    assert!(!outcome.applied);
    assert!(outcome.failed.as_deref().unwrap_or("").contains("enable"));
    assert_eq!(outcome.state, Some(snap(0, 0, 0)));
    assert_eq!(bus.state, snap(0, 0, 0));
}

#[test]
fn rollback_never_restores_zero_thresholds_while_limit_is_enabled() {
    let mut bus = MockBus::new(0, 0, 0);
    bus.ignore_disable = true;
    bus.corrupt_t2_query_once_after_enable = true;

    let outcome = p916f_charge::apply_with_bus(&confirmed(), &mut bus, true, 80, snap(0, 0, 0))
        .expect("transaction report");
    assert!(!outcome.applied);
    assert!(
        outcome
            .rollback
            .as_deref()
            .unwrap_or("")
            .contains("skipped"),
        "dangerous restore was not refused: {:?}",
        outcome.rollback
    );
    assert_eq!(bus.state, snap(1, 80, 100));
    assert!(
        !bus.threshold_values_for(spec::PMC2_CMD_SET_T1).contains(&0),
        "T1=0 written while enabled"
    );
    assert!(
        !bus.threshold_values_for(spec::PMC2_CMD_SET_T2).contains(&0),
        "T2=0 written while enabled (active discharge)"
    );
}

#[test]
fn disable_requires_explicit_zero_readback() {
    let original = snap(1, 80, 100);

    let mut bus = MockBus::new(1, 80, 100);
    bus.ignore_disable = true;
    let outcome = p916f_charge::apply_with_bus(&confirmed(), &mut bus, false, 100, original)
        .expect("transaction report");
    assert!(!outcome.applied);
    assert!(outcome.failed.as_deref().unwrap_or("").contains("0/0/0"));
    assert_eq!(bus.state, original, "failed disable must not claim success");

    let mut bus = MockBus::new(1, 80, 100);
    bus.ignore_disable = true;
    let outcome = p916f_charge::apply_with_bus(&confirmed(), &mut bus, false, 100, original)
        .expect("transaction report");
    assert!(!outcome.applied);
    assert!(outcome.failed.as_deref().unwrap_or("").contains("0/0/0"));
    assert_eq!(outcome.state, Some(original));
    assert_eq!(bus.state, original);
}

#[test]
fn disable_success_reads_back_zero_zero_zero() {
    let mut bus = MockBus::new(1, 80, 100);
    let outcome =
        p916f_charge::apply_with_bus(&confirmed(), &mut bus, false, 100, snap(1, 80, 100))
            .expect("transaction report");
    assert!(outcome.applied);
    assert_eq!(outcome.state, Some(snap(0, 0, 0)));
    assert_eq!(bus.state, snap(0, 0, 0));
}

// ---------------------------------------------------------------------------------------------
// No invalid pair / no active-discharge target
// ---------------------------------------------------------------------------------------------

#[test]
fn enabled_apply_never_writes_t2_below_100() {
    let mut bus = MockBus::new(0, 0, 0);
    let outcome = p916f_charge::apply_with_bus(&confirmed(), &mut bus, true, 80, snap(0, 0, 0))
        .expect("transaction report");
    assert!(outcome.applied);
    assert_eq!(outcome.state, Some(snap(1, 80, 100)));

    let t2_values = bus.threshold_values_for(spec::PMC2_CMD_SET_T2);
    assert_eq!(t2_values, vec![100]);
    assert!(t2_values.iter().all(|value| *value >= 100));
    assert_eq!(bus.threshold_values_for(spec::PMC2_CMD_SET_T1), vec![80]);
}

#[test]
fn existing_85_90_state_is_overwritten_only_by_explicit_apply_with_t2_100() {
    let mut bus = MockBus::new(1, 85, 90);
    let outcome = p916f_charge::apply_with_bus(&confirmed(), &mut bus, true, 80, snap(1, 85, 90))
        .expect("transaction report");
    assert!(outcome.applied);
    assert_eq!(outcome.state, Some(snap(1, 80, 100)));

    let t2_values = bus.threshold_values_for(spec::PMC2_CMD_SET_T2);
    assert_eq!(t2_values, vec![100]);
    assert!(
        !t2_values.contains(&90),
        "original T2=90 was silently rewritten as a target"
    );
}

#[test]
fn numeric_100_and_reversed_pairs_are_never_sent() {
    for threshold in [0u8, 100u8, 101u8] {
        let mut bus = MockBus::new(0, 0, 0);
        let result =
            p916f_charge::apply_with_bus(&confirmed(), &mut bus, true, threshold, snap(0, 0, 0));
        assert!(result.is_err(), "threshold {threshold} was accepted");
        assert!(
            bus.setter_commands().is_empty(),
            "invalid threshold {threshold} reached a setter"
        );
    }
}

#[test]
fn frontend_json_shape_uses_camel_case_snapshot_fields() {
    let mut bus = MockBus::new(1, 80, 100);
    let state = p916f_charge::query_with_bus(&confirmed(), &mut bus);
    let value = serde_json::to_value(state).expect("serialize ChargeState");
    assert!(value.get("threshold1").is_some());
    assert!(value.get("threshold2").is_some());
    assert!(value.get("threshold_1").is_none());

    let expected: NativeChargeSnapshot = serde_json::from_value(serde_json::json!({
        "enabled": 1,
        "threshold1": 80,
        "threshold2": 100
    }))
    .expect("deserialize NativeChargeSnapshot");
    assert_eq!(expected, snap(1, 80, 100));

    let outcome =
        p916f_charge::apply_with_bus(&confirmed(), &mut bus, false, 100, snap(1, 80, 100))
            .expect("transaction report");
    let value = serde_json::to_value(outcome).expect("serialize ApplyOutcome");
    for field in ["applied", "failed", "rollback", "state"] {
        assert!(value.get(field).is_some(), "missing outcome field {field}");
    }
}
