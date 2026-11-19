//! ADLX graphics-feature control model: support/state snapshots, write verification, mutual
//! exclusion handling and a conditional session restore ledger.
//!
//! This module is deliberately free of FFI, Tauri and OS calls so the exact decision logic can be
//! exercised against a scripted mock backend in `tools/native-system-tests`. The only ADLX facts it
//! relies on are the result-code semantics from ADLXDefines.h (`adlx_succeeded`) and the documented
//! AntiLag/Chill mutual exclusion.
//!
//! Honesty rules encoded here:
//! * a feature the driver reports unsupported is never written and never rendered as a control;
//! * an unreadable value stays `None` (unknown), it is never coerced to `false`;
//! * `applied` is true only when every intended write succeeded *and* a fresh readback confirmed
//!   the new value; otherwise the report carries the reason and the actual resulting state.

use serde::{Deserialize, Serialize};

use super::abi::{adlx_result_name, adlx_succeeded};

// ---------------------------------------------------------------------------------------------
// Public state model (serialized straight to the GPU panel)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GfxFeature {
    AntiLag,
    Chill,
    ImageSharpening,
    FrameRateTarget,
}

impl GfxFeature {
    pub fn key(self) -> &'static str {
        match self {
            GfxFeature::AntiLag => "anti_lag",
            GfxFeature::Chill => "chill",
            GfxFeature::ImageSharpening => "image_sharpening",
            GfxFeature::FrameRateTarget => "frame_rate_target",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GfxFeature::AntiLag => "Radeon Anti-Lag",
            GfxFeature::Chill => "Radeon Chill",
            GfxFeature::ImageSharpening => "Radeon Image Sharpening",
            GfxFeature::FrameRateTarget => "Frame Rate Target Control",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "anti_lag" => Some(GfxFeature::AntiLag),
            "chill" => Some(GfxFeature::Chill),
            "image_sharpening" => Some(GfxFeature::ImageSharpening),
            "frame_rate_target" => Some(GfxFeature::FrameRateTarget),
            _ => None,
        }
    }
}

/// Fields this app can change (and therefore may need to restore).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GfxField {
    AntiLag,
    Chill,
    ImageSharpening,
    FrameRateTarget,
    ChillFps,
    FrameRateTargetFps,
}

impl GfxField {
    pub fn key(self) -> &'static str {
        match self {
            GfxField::AntiLag => "anti_lag",
            GfxField::Chill => "chill",
            GfxField::ImageSharpening => "image_sharpening",
            GfxField::FrameRateTarget => "frame_rate_target",
            GfxField::ChillFps => "chill_fps",
            GfxField::FrameRateTargetFps => "frame_rate_target_fps",
        }
    }

    pub fn from_toggle(feature: GfxFeature) -> Self {
        match feature {
            GfxFeature::AntiLag => GfxField::AntiLag,
            GfxFeature::Chill => GfxField::Chill,
            GfxFeature::ImageSharpening => GfxField::ImageSharpening,
            GfxFeature::FrameRateTarget => GfxField::FrameRateTarget,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntRange {
    pub min: i32,
    pub max: i32,
    pub step: i32,
}

impl From<super::abi::AdlxIntRange> for IntRange {
    fn from(r: super::abi::AdlxIntRange) -> Self {
        IntRange {
            min: r.minValue,
            max: r.maxValue,
            step: r.step,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ToggleState {
    pub supported: bool,
    /// `None` = the driver readback failed; the UI must show "unknown", never on/off.
    pub enabled: Option<bool>,
    pub read_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ChillState {
    pub supported: bool,
    pub enabled: Option<bool>,
    pub min_fps: Option<i32>,
    pub max_fps: Option<i32>,
    pub range: Option<IntRange>,
    pub read_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct FrtcState {
    pub supported: bool,
    pub enabled: Option<bool>,
    pub fps: Option<i32>,
    pub range: Option<IntRange>,
    pub read_error: Option<String>,
}

/// Raw `IsSupported*Tuning` answers. Evidence only: this GPU reports all of them false, which is why
/// no clock/voltage/fan/power control exists in the panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct TuningEvidence {
    pub manual_gfx: Option<bool>,
    pub manual_vram: Option<bool>,
    pub manual_fan: Option<bool>,
    pub manual_power: Option<bool>,
    pub auto_tuning: Option<bool>,
    pub preset_tuning: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct SessionInfo {
    /// Fields this app has written since the session started (originals recorded).
    pub changed: Vec<String>,
    /// True when at least one recorded original can still be restored.
    pub can_restore: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct DriverGraphicsState {
    pub available: bool,
    pub detail: Option<String>,
    pub adlx_version: Option<String>,
    pub gpu_name: Option<String>,
    pub anti_lag: ToggleState,
    pub chill: ChillState,
    pub image_sharpening: ToggleState,
    pub frame_rate_target: FrtcState,
    pub enhanced_sync: ToggleState,
    pub tuning: TuningEvidence,
    pub session: SessionInfo,
    pub warnings: Vec<String>,
}

impl DriverGraphicsState {
    /// Honest "nothing is known" state: used when the ADLX DLL is missing, the driver refuses to
    /// initialize, or the runtime version check fails.
    pub fn unavailable(detail: impl Into<String>) -> Self {
        DriverGraphicsState {
            available: false,
            detail: Some(detail.into()),
            ..Default::default()
        }
    }

    /// Uniform view of any feature's toggle projection (Chill/FRTC carry extra fields, so this is
    /// an owned projection rather than a borrow).
    pub fn toggle(&self, feature: GfxFeature) -> ToggleState {
        match feature {
            GfxFeature::AntiLag => self.anti_lag.clone(),
            GfxFeature::ImageSharpening => self.image_sharpening.clone(),
            GfxFeature::Chill => ToggleState {
                supported: self.chill.supported,
                enabled: self.chill.enabled,
                read_error: self.chill.read_error.clone(),
            },
            GfxFeature::FrameRateTarget => ToggleState {
                supported: self.frame_rate_target.supported,
                enabled: self.frame_rate_target.enabled,
                read_error: self.frame_rate_target.read_error.clone(),
            },
        }
    }

    pub fn field_value(&self, field: GfxField) -> Option<FieldValue> {
        match field {
            GfxField::AntiLag => self.anti_lag.enabled.map(FieldValue::Enabled),
            GfxField::Chill => self.chill.enabled.map(FieldValue::Enabled),
            GfxField::ImageSharpening => self.image_sharpening.enabled.map(FieldValue::Enabled),
            GfxField::FrameRateTarget => self.frame_rate_target.enabled.map(FieldValue::Enabled),
            GfxField::ChillFps => match (self.chill.min_fps, self.chill.max_fps) {
                (Some(min), Some(max)) => Some(FieldValue::ChillFps { min, max }),
                _ => None,
            },
            GfxField::FrameRateTargetFps => self.frame_rate_target.fps.map(FieldValue::FrtcFps),
        }
    }
}

/// Field values as actually observed, stored in the restore ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldValue {
    Enabled(bool),
    ChillFps {
        min: i32,
        max: i32,
    },
    FrtcFps(i32),
    /// The original value could not be read, so it must never be "restored" by guessing.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub field: GfxField,
    pub original: FieldValue,
    pub last_written: FieldValue,
}

/// Settings this app changed during the session, with the value observed before the first write.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    pub entries: Vec<LedgerEntry>,
}

impl Ledger {
    pub fn new() -> Self {
        Ledger {
            entries: Vec::new(),
        }
    }

    pub fn get(&self, field: GfxField) -> Option<&LedgerEntry> {
        self.entries.iter().find(|e| e.field == field)
    }

    fn get_mut(&mut self, field: GfxField) -> Option<&mut LedgerEntry> {
        self.entries.iter_mut().find(|e| e.field == field)
    }

    /// Records the pre-write value once; later writes keep the first original.
    pub fn record_original(&mut self, field: GfxField, original: FieldValue) {
        if self.get(field).is_none() {
            self.entries.push(LedgerEntry {
                field,
                original,
                last_written: FieldValue::Unknown,
            });
        }
    }

    pub fn record_written(&mut self, field: GfxField, value: FieldValue) {
        if let Some(entry) = self.get_mut(field) {
            entry.last_written = value;
        } else {
            // No original was captured (should not happen); keep it explicitly unknown.
            self.entries.push(LedgerEntry {
                field,
                original: FieldValue::Unknown,
                last_written: value,
            });
        }
    }

    pub fn changed_keys(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| e.field.key().to_string())
            .collect()
    }

    pub fn can_restore(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.original != FieldValue::Unknown)
    }
}

// ---------------------------------------------------------------------------------------------
// Backend abstraction (implemented by the ADLX session, mocked in tests)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteOutcome {
    pub rc: i32,
    pub detail: Option<String>,
}

impl WriteOutcome {
    pub fn new(rc: i32) -> Self {
        WriteOutcome { rc, detail: None }
    }

    pub fn with_detail(rc: i32, detail: impl Into<String>) -> Self {
        WriteOutcome {
            rc,
            detail: Some(detail.into()),
        }
    }

    pub fn succeeded(&self) -> bool {
        adlx_succeeded(self.rc)
    }

    fn describe(&self) -> String {
        match &self.detail {
            Some(d) => format!("rc={} ({}): {}", self.rc, adlx_result_name(self.rc), d),
            None => format!("rc={} ({})", self.rc, adlx_result_name(self.rc)),
        }
    }
}

/// One live ADLX session's worth of reads and writes.
pub trait GfxBackend {
    fn read_state(&mut self) -> DriverGraphicsState;
    fn write_enabled(&mut self, feature: GfxFeature, enabled: bool) -> WriteOutcome;
    fn write_chill_fps(&mut self, min: i32, max: i32) -> WriteOutcome;
    fn write_frtc_fps(&mut self, fps: i32) -> WriteOutcome;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateReport {
    /// True only for a fully verified change.
    pub applied: bool,
    pub message: Option<String>,
    pub state: DriverGraphicsState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkippedField {
    pub field: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    pub skipped: Vec<SkippedField>,
    pub message: Option<String>,
    pub state: DriverGraphicsState,
}

// ---------------------------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------------------------

pub struct GfxController<B: GfxBackend> {
    backend: B,
    ledger: Ledger,
}

impl<B: GfxBackend> GfxController<B> {
    pub fn new(backend: B, ledger: Ledger) -> Self {
        GfxController { backend, ledger }
    }

    pub fn into_ledger(self) -> Ledger {
        self.ledger
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn state(&mut self) -> DriverGraphicsState {
        let state = self.backend.read_state();
        self.decorate(state)
    }

    fn decorate(&self, mut state: DriverGraphicsState) -> DriverGraphicsState {
        state.session = SessionInfo {
            changed: self.ledger.changed_keys(),
            can_restore: self.ledger.can_restore(),
        };
        state
    }

    /// Applies a toggle, honouring the driver-documented AntiLag/Chill mutual exclusion and
    /// verifying the result by reading the whole state back.
    pub fn apply_toggle(&mut self, feature: GfxFeature, enabled: bool) -> UpdateReport {
        let before = self.backend.read_state();
        let mut problems: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();

        let target = before.toggle(feature);

        if !target.supported {
            let detail = target.read_error.clone().unwrap_or_else(|| {
                "ADLX reports this feature unsupported on the installed GPU".into()
            });
            return UpdateReport {
                applied: false,
                message: Some(format!("{} is not supported: {}", feature.label(), detail)),
                state: self.decorate(before),
            };
        }

        if !enabled && target.enabled == Some(false) {
            // Nothing to change; still report the real state instead of claiming a write.
            return UpdateReport {
                applied: true,
                message: Some(format!("{} is already disabled", feature.label())),
                state: self.decorate(before),
            };
        }
        if enabled && target.enabled == Some(true) {
            return UpdateReport {
                applied: true,
                message: Some(format!("{} is already enabled", feature.label())),
                state: self.decorate(before),
            };
        }

        // Preserve explicit unsupported vs unreadable; the former cannot
        // conflict, the latter is not safe to guess.
        let partner = enabled.then(|| mutual_exclusion_partner(feature)).flatten();
        let mut switched_partner = None;
        if let Some(partner) = partner {
            let state = before.toggle(partner);
            let known_unsupported = !state.supported && state.read_error.is_none();
            match state.enabled {
                Some(true) => {
                    if target.enabled != Some(false) {
                        return UpdateReport { applied: false, message: Some("target state is unreadable; existing mutually exclusive option left unchanged".into()), state: self.decorate(before) };
                    }
                    let field = GfxField::from_toggle(partner);
                    let previous_entry = self.ledger.get(field).cloned();
                    let out = self.backend.write_enabled(partner, false);
                    let mut after = self.backend.read_state();
                    if after.toggle(partner).enabled == Some(false) {
                        self.ledger.record_original(field, FieldValue::Enabled(true));
                        self.ledger.record_written(field, FieldValue::Enabled(false));
                        switched_partner = Some((partner, previous_entry));
                    }
                    if !out.succeeded() || after.toggle(partner).enabled != Some(false) {
                        problems.push(format!("could not confirm {} disabled: {}; primary write not attempted", partner.label(), out.describe()));
                        if let Some((partner, previous)) = switched_partner {
                            after = self.restore_partner_after_failed_switch(feature, partner, previous, after, &mut notes);
                        }
                        return UpdateReport { applied: false, message: join_text(&problems, &notes), state: self.decorate(after) };
                    }
                    notes.push(format!("{} disabled first (driver mutual exclusion)", partner.label()));
                }
                Some(false) => {}
                None if known_unsupported => {}
                None => return UpdateReport {
                    applied: false,
                    message: Some(format!("{} state is unreadable; mutual exclusion cannot be confirmed, no write attempted", partner.label())),
                    state: self.decorate(before),
                },
            }
        }

        let original = match target.enabled {
            Some(v) => FieldValue::Enabled(v),
            None => FieldValue::Unknown,
        };
        self.ledger
            .record_original(GfxField::from_toggle(feature), original);

        let out = self.backend.write_enabled(feature, enabled);
        if out.succeeded() {
            self.ledger
                .record_written(GfxField::from_toggle(feature), FieldValue::Enabled(enabled));
        } else {
            problems.push(format!(
                "the driver refused to set {} {}: {}",
                feature.label(),
                if enabled { "on" } else { "off" },
                out.describe()
            ));
        }

        let mut after = self.backend.read_state();
        let observed = after.toggle(feature).enabled;
        if observed == Some(enabled) {
            self.ledger
                .record_written(GfxField::from_toggle(feature), FieldValue::Enabled(enabled));
        }
        if observed != Some(enabled) {
            problems.push(match observed {
                Some(v) => format!(
                    "readback mismatch: {} reports {}",
                    feature.label(),
                    if v { "enabled" } else { "disabled" }
                ),
                None => format!("readback unavailable: {} state is unknown", feature.label()),
            });
        }

        if let Some(partner) = partner {
            let readback = after.toggle(partner);
            if readback.enabled != Some(false) && (readback.supported || readback.read_error.is_some()) {
                problems.push(format!("{} is not confirmed off", partner.label()));
            }
        }

        if !problems.is_empty() {
            if let Some((partner, previous)) = switched_partner {
                after = self.restore_partner_after_failed_switch(feature, partner, previous, after, &mut notes);
            }
        }

        let message = join_text(&problems, &notes);
        UpdateReport {
            applied: problems.is_empty(),
            message,
            state: self.decorate(after),
        }
    }

    /// Chill min/max FPS. Both values are valid against the driver-reported range.
    fn restore_partner_after_failed_switch(
        &mut self,
        target: GfxFeature,
        partner: GfxFeature,
        previous: Option<LedgerEntry>,
        state: DriverGraphicsState,
        notes: &mut Vec<String>,
    ) -> DriverGraphicsState {
        if state.toggle(target).enabled != Some(false) || state.toggle(partner).enabled != Some(false) {
            notes.push(format!("{} not automatically restored because current states do not confirm that both settings are off", partner.label()));
            return state;
        }
        let _ = self.backend.write_enabled(partner, true);
        let readback = self.backend.read_state();
        if readback.toggle(partner).enabled == Some(true) && readback.toggle(target).enabled == Some(false) {
            let field = GfxField::from_toggle(partner);
            if let Some(entry) = previous {
                if let Some(current) = self.ledger.get_mut(field) { *current = entry; }
            } else {
                self.ledger.entries.retain(|entry| entry.field != field);
            }
            notes.push(format!("{} restored and confirmed", partner.label()));
        } else {
            notes.push(format!("could not confirm {} restored; refresh the driver state", partner.label()));
        }
        readback
    }

    pub fn apply_chill_fps(&mut self, min: i32, max: i32) -> UpdateReport {
        let before = self.backend.read_state();
        let mut problems: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();

        if !before.chill.supported {
            return UpdateReport {
                applied: false,
                message: Some(
                    "Radeon Chill is not supported by the driver, so its FPS range cannot be set"
                        .into(),
                ),
                state: self.decorate(before),
            };
        }
        if min > max {
            // The driver itself reports an equal min/max while Chill holds a fixed frame rate
            // (observed: 60/60 on the 880M), so equality is legal.
            return UpdateReport {
                applied: false,
                message: Some(format!(
                    "invalid range: min FPS ({min}) is above max FPS ({max})"
                )),
                state: self.decorate(before),
            };
        }
        if before.chill.range.is_none() {
            return UpdateReport {
                applied: false,
                message: Some("Chill FPS range is unreadable; no write attempted".into()),
                state: self.decorate(before),
            };
        }
        if let Some(range) = before.chill.range {
            if range.step <= 0
                || min < range.min
                || max > range.max
                || (min - range.min) % range.step != 0
                || (max - range.min) % range.step != 0
            {
                return UpdateReport {
                    applied: false,
                    message: Some(format!(
                        "requested range {min}-{max} FPS is outside the driver range {}-{}",
                        range.min, range.max
                    )),
                    state: self.decorate(before),
                };
            }
        }

        let original = match (before.chill.min_fps, before.chill.max_fps) {
            (Some(min0), Some(max0)) => FieldValue::ChillFps {
                min: min0,
                max: max0,
            },
            _ => FieldValue::Unknown,
        };
        self.ledger.record_original(GfxField::ChillFps, original);

        let out_min = self.backend.write_chill_fps(min, max);
        if out_min.succeeded() {
            self.ledger
                .record_written(GfxField::ChillFps, FieldValue::ChillFps { min, max });
        } else {
            problems.push(format!(
                "the driver refused the Chill FPS range {}: {}",
                range_text(min, max),
                out_min.describe()
            ));
        }

        let after = self.backend.read_state();
        if let (Some(a), Some(b), Some(old_min), Some(old_max)) = (
            after.chill.min_fps,
            after.chill.max_fps,
            before.chill.min_fps,
            before.chill.max_fps,
        ) {
            if (a == min || a == old_min)
                && (b == max || b == old_max)
                && (a != old_min || b != old_max)
            {
                self.ledger
                    .record_written(GfxField::ChillFps, FieldValue::ChillFps { min: a, max: b });
            }
        }
        if after.chill.min_fps != Some(min) || after.chill.max_fps != Some(max) {
            match (after.chill.min_fps, after.chill.max_fps) {
                (Some(a), Some(b)) => problems.push(format!(
                    "readback mismatch: Chill now reports {}",
                    range_text(a, b)
                )),
                _ => problems.push("readback unavailable: Chill FPS range is unknown".into()),
            }
        }
        notes.push("Chill must be enabled for the range to take effect".into());

        UpdateReport {
            applied: problems.is_empty(),
            message: join_text(&problems, &notes),
            state: self.decorate(after),
        }
    }

    /// Frame Rate Target Control FPS value.
    pub fn apply_frtc_fps(&mut self, fps: i32) -> UpdateReport {
        let before = self.backend.read_state();
        let mut problems: Vec<String> = Vec::new();

        if !before.frame_rate_target.supported {
            return UpdateReport {
                applied: false,
                message: Some(
                    "Frame Rate Target Control is not supported by the driver, so its FPS value cannot be set"
                        .into(),
                ),
                state: self.decorate(before),
            };
        }
        if before.frame_rate_target.enabled != Some(true) {
            return UpdateReport {
                applied: false,
                message: Some(
                    "Enable Frame Rate Target Control before setting its FPS; no write attempted"
                        .into(),
                ),
                state: self.decorate(before),
            };
        }
        if before.frame_rate_target.range.is_none() {
            return UpdateReport {
                applied: false,
                message: Some("FRTC FPS range is unreadable; no write attempted".into()),
                state: self.decorate(before),
            };
        }
        if let Some(range) = before.frame_rate_target.range {
            if range.step <= 0
                || fps < range.min
                || fps > range.max
                || (fps - range.min) % range.step != 0
            {
                return UpdateReport {
                    applied: false,
                    message: Some(format!(
                        "requested {fps} FPS is outside the driver range {}-{}",
                        range.min, range.max
                    )),
                    state: self.decorate(before),
                };
            }
        }

        let original = match before.frame_rate_target.fps {
            Some(v) => FieldValue::FrtcFps(v),
            None => FieldValue::Unknown,
        };
        self.ledger
            .record_original(GfxField::FrameRateTargetFps, original);

        let out = self.backend.write_frtc_fps(fps);
        if out.succeeded() {
            self.ledger
                .record_written(GfxField::FrameRateTargetFps, FieldValue::FrtcFps(fps));
        } else {
            problems.push(format!(
                "the driver refused the frame rate target {} FPS: {}",
                fps,
                out.describe()
            ));
        }

        let after = self.backend.read_state();
        if after.frame_rate_target.fps == Some(fps) {
            self.ledger
                .record_written(GfxField::FrameRateTargetFps, FieldValue::FrtcFps(fps));
        }
        if after.frame_rate_target.fps != Some(fps) {
            problems.push(match after.frame_rate_target.fps {
                Some(v) => format!("readback mismatch: frame rate target reports {v} FPS"),
                None => "readback unavailable: frame rate target FPS is unknown".into(),
            });
        }

        UpdateReport {
            applied: problems.is_empty(),
            message: join_text(&problems, &[]),
            state: self.decorate(after),
        }
    }

    /// Restores only the settings this app changed, and only when the value is still the one this
    /// app wrote. External changes are reported and left untouched.
    pub fn restore_session(&mut self) -> RestoreReport {
        let entries = self.ledger.entries.clone();
        let mut restored: Vec<String> = Vec::new();
        let mut skipped: Vec<SkippedField> = Vec::new();
        let mut notes: Vec<String> = Vec::new();

        for entry in entries.into_iter().rev() {
            let field = entry.field;
            let before = self.backend.read_state();
            let current = before.field_value(field);

            if current.is_none() {
                skipped.push(SkippedField {
                    field: field.key().into(),
                    reason: "current value is unreadable, refusing to guess".into(),
                });
                continue;
            }
            if current != Some(entry.last_written) {
                skipped.push(SkippedField {
                    field: field.key().into(),
                    reason: "changed outside this app; left as is".into(),
                });
                continue;
            }
            if entry.original == FieldValue::Unknown {
                skipped.push(SkippedField {
                    field: field.key().into(),
                    reason: "the original value was never readable; nothing to restore".into(),
                });
                continue;
            }

            let outcome = match entry.original {
                FieldValue::Enabled(v) => match toggle_feature(field) {
                    Some(feature) => self.backend.write_enabled(feature, v),
                    None => WriteOutcome::with_detail(-1, "field is not a toggle"),
                },
                FieldValue::ChillFps { min, max } => self.backend.write_chill_fps(min, max),
                FieldValue::FrtcFps(v) => self.backend.write_frtc_fps(v),
                FieldValue::Unknown => unreachable!(),
            };

            if !outcome.succeeded() {
                skipped.push(SkippedField {
                    field: field.key().into(),
                    reason: format!("driver refused the restore: {}", outcome.describe()),
                });
                continue;
            }

            let after = self.backend.read_state();
            if after.field_value(field) == Some(entry.original) {
                restored.push(field.key().to_string());
            } else {
                skipped.push(SkippedField {
                    field: field.key().into(),
                    reason: "restore readback did not confirm the original value".into(),
                });
            }
        }

        // Drop the entries that were fully restored; keep the rest so the user can retry or inspect.
        for key in &restored {
            if let Some(field) = field_from_key(key) {
                self.ledger.entries.retain(|e| e.field != field);
            }
        }

        if !restored.is_empty() {
            notes.push(format!("restored {}", restored.join(", ")));
        }
        if !skipped.is_empty() {
            notes.push(format!("{} setting(s) left unchanged", skipped.len()));
        }

        let state = self.backend.read_state();
        let message = if notes.is_empty() {
            Some("no settings were changed by this app in this session".into())
        } else {
            Some(notes.join("; "))
        };

        RestoreReport {
            restored,
            skipped,
            message,
            state: self.decorate(state),
        }
    }
}

fn mutual_exclusion_partner(feature: GfxFeature) -> Option<GfxFeature> {
    match feature {
        GfxFeature::AntiLag => Some(GfxFeature::Chill),
        GfxFeature::Chill => Some(GfxFeature::AntiLag),
        _ => None,
    }
}

fn toggle_feature(field: GfxField) -> Option<GfxFeature> {
    match field {
        GfxField::AntiLag => Some(GfxFeature::AntiLag),
        GfxField::Chill => Some(GfxFeature::Chill),
        GfxField::ImageSharpening => Some(GfxFeature::ImageSharpening),
        GfxField::FrameRateTarget => Some(GfxFeature::FrameRateTarget),
        _ => None,
    }
}

fn field_from_key(key: &str) -> Option<GfxField> {
    match key {
        "anti_lag" => Some(GfxField::AntiLag),
        "chill" => Some(GfxField::Chill),
        "image_sharpening" => Some(GfxField::ImageSharpening),
        "frame_rate_target" => Some(GfxField::FrameRateTarget),
        "chill_fps" => Some(GfxField::ChillFps),
        "frame_rate_target_fps" => Some(GfxField::FrameRateTargetFps),
        _ => None,
    }
}

fn range_text(min: i32, max: i32) -> String {
    format!("{min}-{max} FPS")
}

fn join_text(problems: &[String], notes: &[String]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    parts.extend(problems.iter().cloned());
    parts.extend(notes.iter().cloned());
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}
