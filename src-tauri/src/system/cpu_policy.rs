//! Windows processor power policy: exact, Plan-GUID-scoped read/write transaction.
//!
//! This module owns only Windows CPU policy (maximum processor state and
//! processor performance boost mode; maximum processor frequency is optional
//! and only reported when the active plan exposes it). It never writes TDP,
//! voltage, EC registers, fan curves, GPU or battery settings, never switches
//! to a different power plan, and never replaces the separate Windows energy
//! overlay from anything but an exact known prior value.
//!
//! Rules encoded here mirror the native usage transaction:
//! * reads are pinned to the exact active plan GUID captured at query time;
//! * every write rechecks that the same plan is still active first;
//! * saved values are not effective until the SAME captured plan is
//!   re-applied with `PowerSetActiveScheme` (never a different plan);
//! * success requires an independent readback of the written value and of the
//!   active plan after activation;
//! * on a partial failure only values this transaction wrote are rolled back,
//!   and only after rechecking the plan and the current value;
//! * a failed readback is reported as unverified, never as restored;
//! * a value changed outside this transaction is never overwritten.
//!
//! The engine has no Tauri or OS dependency so it can be exercised with
//! scripted mocked backends in `tools/native-system-tests`.

use serde::{Deserialize, Serialize};
use std::fmt;

pub const MIN_MAX_PROCESSOR_STATE: u32 = 5;
pub const MAX_MAX_PROCESSOR_STATE: u32 = 100;
pub const MAX_BOOST_MODE: u32 = 6;
pub const MIN_MAX_FREQUENCY_MHZ: u32 = 400;
pub const MAX_MAX_FREQUENCY_MHZ: u32 = 7000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Ac,
    Dc,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Ac => "ac",
            Source::Dc => "dc",
        }
    }
}

/// One independently controlled Windows processor policy value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    MaxProcessorState(Source),
    BoostMode(Source),
    MaxFrequencyMhz(Source),
}

impl Field {
    pub fn name(self) -> &'static str {
        match self {
            Field::MaxProcessorState(Source::Ac) => "ac.max_processor_state",
            Field::MaxProcessorState(Source::Dc) => "dc.max_processor_state",
            Field::BoostMode(Source::Ac) => "ac.boost_mode",
            Field::BoostMode(Source::Dc) => "dc.boost_mode",
            Field::MaxFrequencyMhz(Source::Ac) => "ac.max_frequency_mhz",
            Field::MaxFrequencyMhz(Source::Dc) => "dc.max_frequency_mhz",
        }
    }
}

/// A concrete Windows policy value. `Mhz` is a Windows policy limit, not a
/// promise that the processor reaches or honours that silicon clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Percent(u32),
    Boost(u32),
    Mhz(u32),
}

impl Value {
    pub fn validate(self) -> Result<(), String> {
        match self {
            Value::Percent(value) => {
                if (MIN_MAX_PROCESSOR_STATE..=MAX_MAX_PROCESSOR_STATE).contains(&value) {
                    Ok(())
                } else {
                    Err(format!(
                        "maximum processor state {value}% is outside the supported {MIN_MAX_PROCESSOR_STATE}-{MAX_MAX_PROCESSOR_STATE}% range"
                    ))
                }
            }
            Value::Boost(value) => {
                if value <= MAX_BOOST_MODE {
                    Ok(())
                } else {
                    Err(format!(
                        "processor boost mode {value} is outside the supported 0-{MAX_BOOST_MODE} range"
                    ))
                }
            }
            Value::Mhz(value) => {
                if value == 0 || (MIN_MAX_FREQUENCY_MHZ..=MAX_MAX_FREQUENCY_MHZ).contains(&value) {
                    Ok(())
                } else {
                    Err(format!(
                        "maximum processor frequency {value} MHz is outside 0 (unlimited) or {MIN_MAX_FREQUENCY_MHZ}-{MAX_MAX_FREQUENCY_MHZ} MHz"
                    ))
                }
            }
        }
    }

    pub fn label(self) -> String {
        match self {
            Value::Percent(value) => format!("{value}%"),
            Value::Boost(value) => format!("boost mode {value}"),
            Value::Mhz(value) => format!("{value} MHz policy"),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// Exact values read from one AC or DC side of the active power plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideValues {
    pub max_processor_state: u32,
    pub boost_mode: u32,
    /// `None` means Windows does not expose the maximum-frequency policy for
    /// this side of the active plan. It is never fabricated as a MHz ceiling.
    pub max_mhz: Option<u32>,
}

/// Exact plan identity and values captured by one query. The GUID is the
/// active Windows power scheme; the independent energy overlay is not included
/// and is never changed by this module.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub plan_guid: String,
    pub ac: SideValues,
    pub dc: SideValues,
}

impl Snapshot {
    pub fn max_mhz_supported(&self) -> bool {
        self.ac.max_mhz.is_some() && self.dc.max_mhz.is_some()
    }

    pub fn side(&self, source: Source) -> &SideValues {
        match source {
            Source::Ac => &self.ac,
            Source::Dc => &self.dc,
        }
    }

    pub fn expected(&self, field: Field) -> Option<Value> {
        match field {
            Field::MaxProcessorState(source) => {
                Some(Value::Percent(self.side(source).max_processor_state))
            }
            Field::BoostMode(source) => Some(Value::Boost(self.side(source).boost_mode)),
            Field::MaxFrequencyMhz(source) => self.side(source).max_mhz.map(Value::Mhz),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideUpdate {
    pub max_processor_state: Option<u32>,
    pub boost_mode: Option<u32>,
    pub max_mhz: Option<u32>,
}

/// Requested CPU policy change. Omitted fields are left untouched.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub ac: SideUpdate,
    pub dc: SideUpdate,
}

impl Update {
    pub fn steps(&self, snapshot: &Snapshot) -> Result<Vec<(Field, Value)>, String> {
        let mut steps = Vec::new();
        for (source, side) in [(Source::Ac, &self.ac), (Source::Dc, &self.dc)] {
            if let Some(value) = side.max_processor_state {
                let value = Value::Percent(value);
                value.validate()?;
                steps.push((Field::MaxProcessorState(source), value));
            }
            if let Some(value) = side.boost_mode {
                let value = Value::Boost(value);
                value.validate()?;
                steps.push((Field::BoostMode(source), value));
            }
            if let Some(value) = side.max_mhz {
                if snapshot.side(source).max_mhz.is_none() {
                    return Err(format!(
                        "{} maximum processor frequency is not exposed by the active Windows power plan; it was not written",
                        source.name()
                    ));
                }
                let value = Value::Mhz(value);
                value.validate()?;
                steps.push((Field::MaxFrequencyMhz(source), value));
            }
        }
        if steps.is_empty() {
            return Err("No Windows CPU policy change was requested".into());
        }
        Ok(steps)
    }
}

/// Windows access used by [`execute`]. `read` returns `Ok(None)` only when the
/// setting is genuinely absent from the plan; an unknown readback is `Err`.
pub trait Backend {
    fn active_plan(&mut self) -> Result<String, String>;
    fn read(&mut self, plan: &str, field: Field) -> Result<Option<Value>, String>;
    fn write(&mut self, plan: &str, field: Field, value: &Value) -> Result<(), String>;
    /// Re-apply the exact captured plan. Implementations must not choose or
    /// switch to any other scheme.
    fn activate(&mut self, plan: &str) -> Result<(), String>;
    /// Actual Windows energy overlay, not a requested or inferred mode.
    fn read_overlay(&mut self) -> Result<Option<[u8; 16]>, String>;
    fn restore_overlay(&mut self, overlay: &[u8; 16]) -> Result<(), String>;
}

/// Evidence of one apply attempt. `applied` and `unchanged` describe saved
/// policy values; `activated` is true only after the same plan was re-applied
/// and an independent active-plan readback confirmed it. `applied` values are
/// not effective when `activated` is false.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub plan_guid: String,
    pub applied: Vec<String>,
    pub unchanged: Vec<String>,
    pub failed: Vec<String>,
    pub restored: Vec<String>,
    pub rollback_failed: Vec<String>,
    pub relinquished: Vec<String>,
    pub unverified: Vec<String>,
    /// The exact plan was re-applied and read back as active.
    pub activated: bool,
    pub activation_error: Option<String>,
    /// The same plan was re-applied after rollback to make restored saved
    /// values effective again.
    pub reactivated: bool,
    pub overlay_before: Option<String>,
    pub overlay_after: Option<String>,
    pub overlay_preserved: bool,
    pub overlay_error: Option<String>,
    /// True only when a failure happened and every touched saved field was read
    /// back and confirmed at its original value.
    pub reverted: bool,
}

impl Outcome {
    pub fn succeeded(&self) -> bool {
        self.failed.is_empty()
            && self.rollback_failed.is_empty()
            && self.activation_error.is_none()
            && self.activated
    }
}

/// Run a validated update against the exact snapshot plan. `Err` means
/// validation or preflight failed before any write was attempted.
pub fn execute<B: Backend>(
    backend: &mut B,
    snapshot: &Snapshot,
    update: &Update,
) -> Result<Outcome, String> {
    let steps = update.steps(snapshot)?;
    let active = backend.active_plan()?;
    if active != snapshot.plan_guid {
        return Err(format!(
            "The active Windows power plan changed from {} to {}; no CPU policy write was attempted",
            snapshot.plan_guid, active
        ));
    }

    let mut originals = Vec::with_capacity(steps.len());
    for (field, _) in &steps {
        let current = backend
            .read(&snapshot.plan_guid, *field)
            .map_err(|error| format!("preflight {}: {error}", field.name()))?
            .ok_or_else(|| {
                format!(
                    "preflight {}: this Windows power plan does not expose the setting",
                    field.name()
                )
            })?;
        let expected = snapshot.expected(*field);
        if expected != Some(current) {
            return Err(format!(
                "preflight {}: the plan now reads {current}, not the snapshot value {}; it changed outside this session and no write was attempted",
                field.name(),
                expected.map(|value| value.label()).unwrap_or_else(|| "unavailable".into())
            ));
        }
        originals.push(current);
    }

    let mut outcome = Outcome {
        plan_guid: snapshot.plan_guid.clone(),
        ..Outcome::default()
    };
    let mut failed_at = None;
    for (index, (field, target)) in steps.iter().enumerate() {
        if originals[index] == *target {
            outcome.unchanged.push(field.name().to_string());
            continue;
        }
        let result = match backend.active_plan() {
            Ok(active) if active != snapshot.plan_guid => Err(format!(
                "active Windows power plan changed to {active} before this write; not writing to a different plan"
            )),
            Ok(_) => backend.write(&snapshot.plan_guid, *field, target).and_then(|()| {
                let actual = backend
                    .read(&snapshot.plan_guid, *field)?
                    .ok_or_else(|| "readback says the setting is not exposed".to_string())?;
                if actual == *target {
                    Ok(())
                } else {
                    Err(format!("readback mismatch (expected {target}, got {actual})"))
                }
            }),
            Err(error) => Err(format!("could not recheck the active Windows power plan: {error}")),
        };
        match result {
            Ok(()) => outcome.applied.push(field.name().to_string()),
            Err(error) => {
                outcome.failed.push(format!("{}: {error}", field.name()));
                failed_at = Some(index);
                break;
            }
        }
    }

    if let Some(last) = failed_at {
        rollback_steps(backend, snapshot, &steps, &originals, last, &mut outcome);
        finish_rollback(&mut outcome);
        return Ok(outcome);
    }

    // Saved values are ineffective until the SAME plan is re-applied. The
    // helper never selects another scheme and preserves the independent
    // energy overlay when it can be read exactly.
    match activate_same_plan(backend, snapshot, &mut outcome) {
        Ok(()) => {
            outcome.activated = true;
            return Ok(outcome);
        }
        Err(error) => {
            outcome.activation_error = Some(error.clone());
            outcome.failed.push(format!("activation: {error}"));
        }
    }

    rollback_steps(
        backend,
        snapshot,
        &steps,
        &originals,
        steps.len().saturating_sub(1),
        &mut outcome,
    );
    finish_rollback(&mut outcome);

    // Restoring saved values needs the same plan re-applied again so the
    // original policy is effective, not merely stored.
    if outcome.reverted {
        match backend.active_plan() {
            Ok(active) if active == snapshot.plan_guid => {
                match activate_same_plan(backend, snapshot, &mut outcome) {
                    Ok(()) => outcome.reactivated = true,
                    Err(error) => {
                        outcome
                            .rollback_failed
                            .push(format!("reactivation: {error}"));
                        outcome.reverted = false;
                    }
                }
            }
            Ok(active) => {
                outcome.rollback_failed.push(format!(
                    "reactivation: active Windows power plan is {active}, not {}; refusing to switch plans",
                    snapshot.plan_guid
                ));
                outcome.reverted = false;
            }
            Err(error) => {
                outcome.rollback_failed.push(format!(
                    "reactivation: active Windows power plan could not be rechecked ({error})"
                ));
                outcome.reverted = false;
            }
        }
    }
    Ok(outcome)
}

fn overlay_text(overlay: &[u8; 16]) -> String {
    overlay.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn preserve_overlay<B: Backend>(
    backend: &mut B,
    before: Option<[u8; 16]>,
    after: Option<[u8; 16]>,
    outcome: &mut Outcome,
) {
    match (before, after) {
        (Some(before), Some(after)) if before != after => match backend.restore_overlay(&before) {
            Ok(()) => match backend.read_overlay() {
                Ok(Some(now)) if now == before => outcome.overlay_preserved = true,
                Ok(Some(now)) => {
                    outcome.overlay_error = Some(format!(
                        "overlay restore readback mismatch (expected {}, got {})",
                        overlay_text(&before),
                        overlay_text(&now)
                    ))
                }
                Ok(None) => {
                    outcome.overlay_error =
                        Some("overlay restore readback is unavailable; not overwriting it".into())
                }
                Err(error) => {
                    outcome.overlay_error =
                        Some(format!("overlay restore readback failed: {error}"))
                }
            },
            Err(error) => {
                outcome.overlay_error =
                    Some(format!("could not restore independent overlay: {error}"))
            }
        },
        (Some(_), Some(_)) => outcome.overlay_preserved = true,
        _ => {
            if outcome.overlay_error.is_none() {
                outcome.overlay_error =
                    Some("overlay state is unknown; it was not overwritten".into());
            }
        }
    }
}

fn activate_same_plan<B: Backend>(
    backend: &mut B,
    snapshot: &Snapshot,
    outcome: &mut Outcome,
) -> Result<(), String> {
    match backend.active_plan() {
        Ok(active) if active == snapshot.plan_guid => {}
        Ok(active) => {
            return Err(format!(
                "active Windows power plan changed to {active} before activation; not activating a different plan"
            ))
        }
        Err(error) => {
            return Err(format!(
                "active Windows power plan could not be rechecked before activation ({error})"
            ))
        }
    }

    let before = match backend.read_overlay() {
        Ok(overlay) => overlay,
        Err(error) => {
            outcome.overlay_error = Some(format!("overlay read before activation failed: {error}"));
            None
        }
    };
    outcome.overlay_before = before.as_ref().map(overlay_text);
    let activation = backend.activate(&snapshot.plan_guid);
    let active_after = backend.active_plan();
    let after = match backend.read_overlay() {
        Ok(overlay) => overlay,
        Err(error) => {
            outcome.overlay_error = Some(format!("overlay read after activation failed: {error}"));
            None
        }
    };
    outcome.overlay_after = after.as_ref().map(overlay_text);

    match active_after {
        Ok(active) if active == snapshot.plan_guid => {
            preserve_overlay(backend, before, after, outcome);
        }
        Ok(active) => {
            return Err(format!(
            "active Windows power plan changed to {active} during activation; not switching back"
        ))
        }
        Err(error) => {
            return Err(format!(
                "active Windows power plan readback after activation failed: {error}"
            ))
        }
    }

    activation.map_err(|error| format!("PowerSetActiveScheme(same plan) failed: {error}"))
}

fn rollback_steps<B: Backend>(
    backend: &mut B,
    snapshot: &Snapshot,
    steps: &[(Field, Value)],
    originals: &[Value],
    last: usize,
    outcome: &mut Outcome,
) {
    for index in (0..=last).rev() {
        let (field, target) = &steps[index];
        if originals[index] == *target {
            continue;
        }
        let name = field.name();
        match backend.active_plan() {
            Ok(active) if active != snapshot.plan_guid => {
                outcome.rollback_failed.push(format!(
                    "{name}: active Windows power plan changed to {active}; refusing to restore a value on a different plan"
                ));
                continue;
            }
            Err(error) => {
                outcome.rollback_failed.push(format!(
                    "{name}: could not recheck the active Windows power plan ({error}); no restore attempted"
                ));
                continue;
            }
            Ok(_) => {}
        }
        let current = match backend.read(&snapshot.plan_guid, *field) {
            Ok(Some(value)) => value,
            Ok(None) => {
                outcome.unverified.push(format!(
                    "{name}: readback says the setting is no longer exposed; value not verified or restored"
                ));
                continue;
            }
            Err(error) => {
                outcome.unverified.push(format!(
                    "{name}: readback failed ({error}); value not verified or restored"
                ));
                continue;
            }
        };
        if current == originals[index] {
            continue;
        }
        if current != *target {
            outcome.relinquished.push(format!(
                "{name}: now {current}, which is neither the requested {target} nor the original {}; left untouched",
                originals[index]
            ));
            continue;
        }
        let restored = backend
            .write(&snapshot.plan_guid, *field, &originals[index])
            .and_then(|()| {
                let actual = backend.read(&snapshot.plan_guid, *field)?.ok_or_else(|| {
                    "restore readback says the setting is not exposed".to_string()
                })?;
                if actual == originals[index] {
                    Ok(())
                } else {
                    Err(format!(
                        "restore readback mismatch (expected {}, got {actual})",
                        originals[index]
                    ))
                }
            });
        match restored {
            Ok(()) => outcome.restored.push(name.to_string()),
            Err(error) => outcome.rollback_failed.push(format!(
                "{name}: could not restore {} ({error})",
                originals[index]
            )),
        }
    }
}

fn finish_rollback(outcome: &mut Outcome) {
    outcome.reverted = outcome.rollback_failed.is_empty()
        && outcome.relinquished.is_empty()
        && outcome.unverified.is_empty();
}

/// Build the full update that restores a session snapshot.
pub fn restore_update(snapshot: &Snapshot) -> Update {
    let side = |values: &SideValues| SideUpdate {
        max_processor_state: Some(values.max_processor_state),
        boost_mode: Some(values.boost_mode),
        max_mhz: values.max_mhz,
    };
    Update {
        ac: side(&snapshot.ac),
        dc: side(&snapshot.dc),
    }
}
