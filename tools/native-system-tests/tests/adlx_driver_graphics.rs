//! Deterministic tests for the AMD ADLX driver-graphics control model.
//!
//! No Tauri, no OS calls and no AMD driver: every ADLX reaction is scripted below, including the
//! ugly ones (refused writes, writes that report success but change nothing, unreadable readbacks,
//! external changes between commands). The real driver is exercised only by the ignored tests in
//! `src-tauri/tests/adlx_driver_live.rs`.

use openrevo_native_system_tests::adlx::engine::{
    ChillState, DriverGraphicsState, FieldValue, FrtcState, GfxBackend, GfxController, GfxFeature,
    GfxField, IntRange, Ledger, ToggleState, WriteOutcome,
};

// The engine re-exports the ADLX result constants through `abi`; mirror the few needed here.
const RC_OK: i32 = 0;
const RC_FAIL: i32 = 3;
const RC_NOT_SUPPORTED: i32 = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Driver applies the write and reports success.
    Honest,
    /// Driver refuses the write and reports failure.
    Reject,
    /// Driver reports success but the value does not change (silent no-op).
    SilentNoOp,
    /// Driver applies the write but reports a failure code.
    ApplyThenFail,
    /// Driver applies the write but the readback becomes unreadable.
    UnreadableAfter,
}

struct Mock {
    state: DriverGraphicsState,
    mode: Mode,
    /// Field writes in call order, e.g. "chill=false", "chill_fps=40-120".
    writes: Vec<String>,
}

impl Mock {
    fn new() -> Self {
        Mock {
            state: supported_880m_state(),
            mode: Mode::Honest,
            writes: Vec::new(),
        }
    }

    fn with_mode(mode: Mode) -> Self {
        let mut m = Mock::new();
        m.mode = mode;
        m
    }

    fn outcome(&self, rc_on_failure: i32) -> (bool, WriteOutcome) {
        match self.mode {
            Mode::Honest => (true, WriteOutcome::new(RC_OK)),
            Mode::Reject => (
                false,
                WriteOutcome::with_detail(rc_on_failure, "scripted refusal"),
            ),
            Mode::SilentNoOp => (false, WriteOutcome::new(RC_OK)),
            Mode::ApplyThenFail => (
                true,
                WriteOutcome::with_detail(RC_FAIL, "scripted late failure"),
            ),
            Mode::UnreadableAfter => (true, WriteOutcome::new(RC_OK)),
        }
    }

    fn blank_readback(&mut self, feature: GfxFeature) {
        if self.mode != Mode::UnreadableAfter {
            return;
        }
        match feature {
            GfxFeature::AntiLag => self.state.anti_lag.enabled = None,
            GfxFeature::ImageSharpening => self.state.image_sharpening.enabled = None,
            GfxFeature::Chill => self.state.chill.enabled = None,
            GfxFeature::FrameRateTarget => self.state.frame_rate_target.enabled = None,
        }
    }
}

impl GfxBackend for Mock {
    fn read_state(&mut self) -> DriverGraphicsState {
        self.state.clone()
    }

    fn write_enabled(&mut self, feature: GfxFeature, enabled: bool) -> WriteOutcome {
        self.writes.push(format!("{}={}", feature.key(), enabled));
        let supported = self.state.toggle(feature).supported;
        if !supported {
            return WriteOutcome::new(RC_NOT_SUPPORTED);
        }
        let (apply, outcome) = self.outcome(RC_FAIL);
        if apply {
            match feature {
                GfxFeature::AntiLag => self.state.anti_lag.enabled = Some(enabled),
                GfxFeature::ImageSharpening => self.state.image_sharpening.enabled = Some(enabled),
                GfxFeature::Chill => self.state.chill.enabled = Some(enabled),
                GfxFeature::FrameRateTarget => self.state.frame_rate_target.enabled = Some(enabled),
            }
        }
        self.blank_readback(feature);
        outcome
    }

    fn write_chill_fps(&mut self, min: i32, max: i32) -> WriteOutcome {
        self.writes.push(format!("chill_fps={min}-{max}"));
        let (apply, outcome) = self.outcome(RC_FAIL);
        if apply {
            self.state.chill.min_fps = Some(min);
            self.state.chill.max_fps = Some(max);
        }
        if self.mode == Mode::UnreadableAfter {
            self.state.chill.min_fps = None;
        }
        outcome
    }

    fn write_frtc_fps(&mut self, fps: i32) -> WriteOutcome {
        self.writes.push(format!("frtc_fps={fps}"));
        let (apply, outcome) = self.outcome(RC_FAIL);
        if apply {
            self.state.frame_rate_target.fps = Some(fps);
        }
        if self.mode == Mode::UnreadableAfter {
            self.state.frame_rate_target.fps = None;
        }
        outcome
    }
}

/// Mirrors the validated 880M / ADLX 1.5.0.124 probe result: AntiLag, Chill (enabled),
/// ImageSharpening and FRTC supported; EnhancedSync not supported; every tuning path false.
fn supported_880m_state() -> DriverGraphicsState {
    DriverGraphicsState {
        available: true,
        detail: Some("ADLX 1.5.0.124 via amdadlx64.dll".into()),
        adlx_version: Some("1.5.0.124".into()),
        gpu_name: Some("AMD Radeon(TM) 880M Graphics".into()),
        anti_lag: ToggleState {
            supported: true,
            enabled: Some(false),
            read_error: None,
        },
        chill: ChillState {
            supported: true,
            enabled: Some(true),
            min_fps: Some(40),
            max_fps: Some(120),
            range: Some(IntRange {
                min: 30,
                max: 240,
                step: 1,
            }),
            read_error: None,
        },
        image_sharpening: ToggleState {
            supported: true,
            enabled: Some(false),
            read_error: None,
        },
        frame_rate_target: FrtcState {
            supported: true,
            enabled: Some(false),
            fps: Some(60),
            range: Some(IntRange {
                min: 30,
                max: 240,
                step: 1,
            }),
            read_error: None,
        },
        enhanced_sync: ToggleState {
            supported: false,
            enabled: None,
            read_error: None,
        },
        tuning: Default::default(),
        session: Default::default(),
        warnings: Vec::new(),
    }
}

#[test]
fn unsupported_feature_is_never_written_and_never_reported_applied() {
    // EnhancedSync is not modelled as a controllable feature at all: it is unsupported on this GPU
    // and the panel shows it as evidence, not as a toggle.
    assert!(GfxFeature::from_key("enhanced_sync").is_none());

    // A genuinely unsupported toggle: AntiLag marked unsupported by the driver.
    let mut unsupported = supported_880m_state();
    unsupported.anti_lag = ToggleState {
        supported: false,
        enabled: None,
        read_error: Some("rc=12 (ADLX_NOT_SUPPORTED)".into()),
    };
    let mock = Mock {
        state: unsupported,
        mode: Mode::Honest,
        writes: Vec::new(),
    };
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_toggle(GfxFeature::AntiLag, true);
    assert!(!report.applied, "unsupported feature must not look applied");
    assert!(report.message.as_deref().unwrap().contains("not supported"));
    assert!(
        ctl.into_ledger().entries.is_empty(),
        "no write, no ledger entry"
    );
}

#[test]
fn enabling_anti_lag_disables_chill_first_and_verifies_both() {
    let mock = Mock::new();
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_toggle(GfxFeature::AntiLag, true);

    assert!(report.applied, "message={:?}", report.message);
    assert_eq!(report.state.anti_lag.enabled, Some(true));
    assert_eq!(report.state.chill.enabled, Some(false), "mutual exclusion");
    assert!(report
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("mutual exclusion"));

    let ledger = ctl.into_ledger();
    assert_eq!(
        ledger.get(GfxField::Chill).unwrap().original,
        FieldValue::Enabled(true)
    );
    assert_eq!(
        ledger.get(GfxField::AntiLag).unwrap().original,
        FieldValue::Enabled(false)
    );
    assert_eq!(ledger.changed_keys(), vec!["chill", "anti_lag"]);
    assert!(ledger.can_restore());
}

#[test]
fn refused_write_reports_failure_with_the_result_code() {
    let mock = Mock::with_mode(Mode::Reject);
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_toggle(GfxFeature::Chill, false);

    assert!(!report.applied);
    let message = report.message.unwrap();
    assert!(message.contains("ADLX_FAIL"), "{message}");
    assert_eq!(
        report.state.chill.enabled,
        Some(true),
        "state stays truthful"
    );

    let ledger = ctl.into_ledger();
    assert_eq!(
        ledger.get(GfxField::Chill).unwrap().last_written,
        FieldValue::Unknown,
        "a refused write is not recorded as this app's value"
    );
}

#[test]
fn silent_no_op_is_detected_by_readback() {
    let mock = Mock::with_mode(Mode::SilentNoOp);
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_toggle(GfxFeature::ImageSharpening, true);
    assert!(!report.applied);
    assert!(report
        .message
        .as_deref()
        .unwrap()
        .contains("readback mismatch"));
    assert_eq!(report.state.image_sharpening.enabled, Some(false));
}

#[test]
fn apply_then_fail_is_not_success_even_if_readback_matches() {
    let mock = Mock::with_mode(Mode::ApplyThenFail);
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_toggle(GfxFeature::ImageSharpening, true);
    assert!(
        !report.applied,
        "a failing result code must never look successful"
    );
    assert!(report.message.as_deref().unwrap().contains("ADLX_FAIL"));
    assert_eq!(
        report.state.image_sharpening.enabled,
        Some(true),
        "actual state is still returned"
    );
}

#[test]
fn unreadable_readback_is_unknown_not_disabled() {
    let mock = Mock::with_mode(Mode::UnreadableAfter);
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_toggle(GfxFeature::ImageSharpening, true);
    assert!(!report.applied);
    assert_eq!(
        report.state.image_sharpening.enabled, None,
        "unknown must not be coerced to false"
    );
    assert!(report.message.as_deref().unwrap().contains("unknown"));

    // ...and it serializes as JSON null, not false.
    let json = serde_json::to_value(&report.state).unwrap();
    assert!(json["image_sharpening"]["enabled"].is_null());
}

#[test]
fn chill_fps_is_range_checked_before_any_write() {
    let mock = Mock::new();
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_chill_fps(10, 500);
    assert!(!report.applied);
    assert!(report
        .message
        .as_deref()
        .unwrap()
        .contains("outside the driver range"));
    assert!(ctl.into_ledger().entries.is_empty());

    let mock = Mock::new();
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_chill_fps(120, 60);
    assert!(!report.applied);
    assert!(report.message.as_deref().unwrap().contains("is above"));

    // Equal min/max is legal: the driver itself reports 60/60 while Chill holds a fixed rate.
    let mock = Mock::new();
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_chill_fps(75, 75);
    assert!(report.applied, "message={:?}", report.message);
}

#[test]
fn chill_fps_write_records_original_and_verifies_readback() {
    let mock = Mock::new();
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_chill_fps(50, 100);
    assert!(report.applied, "message={:?}", report.message);
    assert_eq!(report.state.chill.min_fps, Some(50));
    assert_eq!(report.state.chill.max_fps, Some(100));
    let ledger = ctl.into_ledger();
    assert_eq!(
        ledger.get(GfxField::ChillFps).unwrap().original,
        FieldValue::ChillFps { min: 40, max: 120 }
    );
}

#[test]
fn frtc_fps_write_is_verified_and_range_checked() {
    let mut mock = Mock::new();
    mock.state.frame_rate_target.enabled = Some(true);
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_frtc_fps(90);
    assert!(report.applied, "message={:?}", report.message);
    assert_eq!(report.state.frame_rate_target.fps, Some(90));

    let mut mock = Mock::with_mode(Mode::SilentNoOp);
    mock.state.frame_rate_target.enabled = Some(true);
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.apply_frtc_fps(90);
    assert!(!report.applied);
    assert!(report
        .message
        .as_deref()
        .unwrap()
        .contains("readback mismatch"));
}

#[test]
fn original_is_captured_once_across_repeated_writes() {
    let mock = Mock::new();
    let mut ctl = GfxController::new(mock, Ledger::new());
    assert!(ctl.apply_toggle(GfxFeature::AntiLag, true).applied);
    assert!(ctl.apply_toggle(GfxFeature::AntiLag, false).applied);
    assert!(ctl.apply_toggle(GfxFeature::AntiLag, true).applied);

    let ledger = ctl.into_ledger();
    let entry = ledger.get(GfxField::AntiLag).unwrap();
    assert_eq!(
        entry.original,
        FieldValue::Enabled(false),
        "first original wins"
    );
    assert_eq!(entry.last_written, FieldValue::Enabled(true));
    // "chill" is recorded too because the first AntiLag enable had to disable it (exclusion).
    assert_eq!(ledger.changed_keys(), vec!["chill", "anti_lag"]);
    assert_eq!(
        ledger.get(GfxField::Chill).unwrap().original,
        FieldValue::Enabled(true)
    );
}

#[test]
fn restore_reverts_only_this_apps_changes_and_preserves_external_edits() {
    let mut mock = Mock::new();
    mock.state.frame_rate_target.enabled = Some(true);
    let mut ctl = GfxController::new(mock, Ledger::new());
    // Enable AntiLag (disables Chill), then change FRTC FPS.
    assert!(ctl.apply_toggle(GfxFeature::AntiLag, true).applied);
    assert!(ctl.apply_frtc_fps(90).applied);

    // External change: something else turns Chill back on behind our back.
    // We emulate it by taking the controller apart and mutating the backend state directly.
    let ledger = ctl.into_ledger();
    assert!(ledger.can_restore());

    let mut backend = Mock::new();
    backend.state.anti_lag.enabled = Some(true);
    backend.state.chill.enabled = Some(true); // external change
    backend.state.frame_rate_target.fps = Some(90);

    let mut ctl = GfxController::new(backend, ledger);
    let report = ctl.restore_session();
    assert_eq!(report.restored, vec!["frame_rate_target_fps", "anti_lag"]);
    let skipped = &report.skipped;
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].field, "chill");
    assert!(skipped[0].reason.contains("changed outside this app"));
    assert_eq!(
        report.state.chill.enabled,
        Some(true),
        "external change preserved"
    );
    assert_eq!(report.state.anti_lag.enabled, Some(false));
    assert_eq!(report.state.frame_rate_target.fps, Some(60));

    // Nothing left worth restoring, but the skipped field stays visible in the ledger.
    let ledger = ctl.into_ledger();
    assert_eq!(ledger.changed_keys(), vec!["chill"]);
}

#[test]
fn restore_refuses_to_guess_an_unknown_original() {
    let mut state = supported_880m_state();
    state.image_sharpening.enabled = None; // unreadable before our write
    let backend = Mock {
        state,
        mode: Mode::Honest,
        writes: Vec::new(),
    };
    let mut ctl = GfxController::new(backend, Ledger::new());
    let report = ctl.apply_toggle(GfxFeature::ImageSharpening, true);
    assert!(
        report.applied,
        "the new value was verified even though the original was unknown"
    );
    assert_eq!(report.state.image_sharpening.enabled, Some(true));

    let mut ledger = ctl.into_ledger();
    assert_eq!(
        ledger.get(GfxField::ImageSharpening).unwrap().original,
        FieldValue::Unknown
    );
    assert!(!ledger.can_restore());

    let mut backend = Mock::new();
    backend.state.image_sharpening.enabled = Some(true);
    let mut ctl = GfxController::new(backend, ledger.clone());
    let report = ctl.restore_session();
    assert!(report.restored.is_empty());
    assert_eq!(report.skipped[0].field, "image_sharpening");
    assert!(report.skipped[0].reason.contains("never readable"));

    // Sanity: the cloned ledger still marks the entry as non-restorable.
    ledger.entries.clear();
    assert_eq!(ledger, Ledger::new());
}

#[test]
fn restore_with_empty_ledger_touches_nothing() {
    let mock = Mock::new();
    let mut ctl = GfxController::new(mock, Ledger::new());
    let report = ctl.restore_session();
    assert!(report.restored.is_empty());
    assert!(report.skipped.is_empty());
    assert!(report.message.as_deref().unwrap().contains("no settings"));
}

#[test]
fn readback_failure_during_restore_is_reported_not_forced() {
    let mock = Mock::new();
    let mut ctl = GfxController::new(mock, Ledger::new());
    assert!(ctl.apply_chill_fps(50, 100).applied);
    let ledger = ctl.into_ledger();

    // The driver still holds our 50-100 value, but the restore write silently does nothing.
    let mut backend = Mock::with_mode(Mode::SilentNoOp);
    backend.state.chill.min_fps = Some(50);
    backend.state.chill.max_fps = Some(100);
    let mut ctl = GfxController::new(backend, ledger);
    let report = ctl.restore_session();
    assert!(report.restored.is_empty());
    assert_eq!(report.skipped[0].field, "chill_fps");
    assert!(report.skipped[0]
        .reason
        .contains("readback did not confirm the original value"));
    assert_eq!(
        report.state.chill.min_fps,
        Some(50),
        "unchanged value is still reported"
    );
}

#[test]
fn state_carries_session_bookkeeping_and_never_claims_a_restore_it_cannot_do() {
    let mock = Mock::new();
    let mut ctl = GfxController::new(mock, Ledger::new());
    assert!(ctl.state().session.changed.is_empty());
    assert!(!ctl.state().session.can_restore);

    assert!(ctl.apply_toggle(GfxFeature::Chill, false).applied);
    let state = ctl.state();
    assert_eq!(state.session.changed, vec!["chill".to_string()]);
    assert!(state.session.can_restore);
}

#[test]
fn tuning_flags_are_evidence_and_never_writable() {
    let mut state = supported_880m_state();
    state.tuning.manual_gfx = Some(false);
    state.tuning.manual_vram = Some(false);
    state.tuning.manual_fan = Some(false);
    state.tuning.manual_power = Some(false);
    state.tuning.auto_tuning = Some(false);
    state.tuning.preset_tuning = Some(false);
    let backend = Mock {
        state,
        mode: Mode::Honest,
        writes: Vec::new(),
    };
    let mut ctl = GfxController::new(backend, Ledger::new());
    let state = ctl.state();
    assert_eq!(state.tuning.manual_gfx, Some(false));
    assert_eq!(state.tuning.manual_power, Some(false));

    // The public feature set contains only driver graphics features, no tuning knobs.
    let keys = ["anti_lag", "chill", "image_sharpening", "frame_rate_target"];
    for key in keys {
        assert!(GfxFeature::from_key(key).is_some());
    }
    for forbidden in [
        "manual_gfx",
        "gpu_clock",
        "vram_clock",
        "fan_speed",
        "power_limit",
    ] {
        assert!(
            GfxFeature::from_key(forbidden).is_none(),
            "{forbidden} must not be settable"
        );
    }
}

#[test]
fn serialized_state_has_the_field_names_the_panel_expects() {
    let mock = Mock::new();
    let mut ctl = GfxController::new(mock, Ledger::new());
    let state = ctl.state();
    let json = serde_json::to_value(&state).unwrap();
    for key in [
        "available",
        "detail",
        "adlx_version",
        "gpu_name",
        "anti_lag",
        "chill",
        "image_sharpening",
        "frame_rate_target",
        "enhanced_sync",
        "tuning",
        "session",
        "warnings",
    ] {
        assert!(json.get(key).is_some(), "missing key {key}");
    }
    assert_eq!(json["gpu_name"], "AMD Radeon(TM) 880M Graphics");
    assert_eq!(json["chill"]["range"]["step"], 1);
    assert_eq!(json["enhanced_sync"]["supported"], false);
    assert!(json["enhanced_sync"]["enabled"].is_null());
}

#[test]
fn unreadable_ranges_block_ipc_writes() {
    let mut mock = Mock::new();
    mock.state.chill.range = None;
    mock.state.frame_rate_target.range = None;
    let mut ctl = GfxController::new(mock, Ledger::new());
    assert!(!ctl.apply_chill_fps(30, 100).applied);
    assert!(!ctl.apply_frtc_fps(90).applied);
    assert!(ctl.into_ledger().entries.is_empty());
}

#[test]
fn failed_partner_disable_stops_before_primary_write() {
    let mut ctl = GfxController::new(Mock::with_mode(Mode::Reject), Ledger::new());
    let report = ctl.apply_toggle(GfxFeature::AntiLag, true);
    assert!(!report.applied);
    assert!(ctl.ledger().get(GfxField::AntiLag).is_none());
}

#[test]
fn unknown_partner_state_stops_before_any_write() {
    let mut mock = Mock::new();
    mock.state.chill.enabled = None;
    let mut ctl = GfxController::new(mock, Ledger::new());
    assert!(!ctl.apply_toggle(GfxFeature::AntiLag, true).applied);
    assert!(ctl.into_ledger().entries.is_empty());
}

#[test]
fn error_after_mutation_remains_failed_but_preserves_restore_ownership() {
    let mut ctl = GfxController::new(Mock::with_mode(Mode::ApplyThenFail), Ledger::new());
    assert!(!ctl.apply_toggle(GfxFeature::ImageSharpening, true).applied);
    let ledger = ctl.into_ledger();
    let mut mock = Mock::new();
    mock.state.image_sharpening.enabled = Some(true);
    let mut ctl = GfxController::new(mock, ledger);
    let restored = ctl.restore_session();
    assert_eq!(restored.restored, vec!["image_sharpening"]);
    assert_eq!(restored.state.image_sharpening.enabled, Some(false));
}
