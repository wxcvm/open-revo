//! Pure transaction engine for applying a native STX usage profile.
//!
//! Rules encoded here:
//! * every field is read before the first hardware write (preflight);
//! * writes run in a fixed order and stop at the first failure;
//! * success is only claimed after an independent readback of the written field;
//! * after a failure only fields that still hold the value this transaction
//!   wrote are reverted, and a revert is only reported when readback confirms
//!   the original value;
//! * a failed readback is reported as unverified, never as restored;
//! * a value that matches neither the original nor the requested value belongs
//!   to someone else and is never overwritten.
//!
//! The engine has no Tauri, hardware or OS dependency so the transaction can be
//! exercised with a mocked backend in `tools/native-system-tests`.

use std::fmt;

/// One independently controlled setting of a usage profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Firmware,
    Energy,
    Brightness,
    KeyboardTimeout,
    RefreshRate,
}

impl Field {
    pub fn name(self) -> &'static str {
        match self {
            Field::Firmware => "firmware",
            Field::Energy => "windows_energy",
            Field::Brightness => "brightness",
            Field::KeyboardTimeout => "keyboard_timeout",
            Field::RefreshRate => "refresh_rate",
        }
    }
}

/// A concrete value of one [`Field`]. Variants are opaque to the engine; the
/// backend owns the conversion to firmware/Windows values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Mode(u8),
    /// Raw power-scheme GUID bytes, kept exact so unknown schemes can be restored.
    Energy([u8; 16]),
    Brightness(u8),
    Timer(u32),
    Refresh(u32),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Mode(v) => write!(f, "mode {v}"),
            Value::Energy(bytes) => {
                write!(f, "energy ")?;
                for byte in bytes {
                    write!(f, "{byte:02x}")?;
                }
                Ok(())
            }
            Value::Brightness(v) => write!(f, "brightness {v}%"),
            Value::Timer(v) => write!(f, "timeout {v}s"),
            Value::Refresh(v) => write!(f, "{v} Hz"),
        }
    }
}

/// Hardware access used by [`execute`]. Scalar fields only need read/write;
/// fields that cannot be restored by value (display refresh with DRR or an
/// earlier override) supply an opaque checkpoint plus a dedicated rollback.
pub trait Backend {
    /// Exact pre-transaction state for fields with dedicated rollback.
    type Checkpoint;
    fn read(&mut self, field: Field) -> Result<Value, String>;
    fn write(&mut self, field: Field, value: &Value) -> Result<(), String>;
    /// Capture dedicated rollback state before the first write. `None` means
    /// the engine can roll the field back by writing the preflight value.
    fn checkpoint(&mut self, _field: Field) -> Result<Option<Self::Checkpoint>, String> {
        Ok(None)
    }
    /// Restore the exact pre-transaction state and verify it. The backend owns
    /// the ownership decision; it must refuse rather than overwrite a value
    /// changed outside this transaction.
    fn rollback(&mut self, field: Field, original: &Value, _checkpoint: Option<&Self::Checkpoint>) -> Result<(), String> {
        self.write(field, original)?;
        let actual = self.read(field)?;
        if actual == *original {
            Ok(())
        } else {
            Err(format!("restore readback mismatch (expected {original}, got {actual})"))
        }
    }
}

/// Which transaction owns the current state of a check-pointed field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ownership {
    /// The field never left its pre-transaction state.
    Unchanged,
    /// The state was written by this transaction.
    Ours,
    /// Someone else changed it; rollback must refuse.
    External,
}

/// Pure ownership decision shared by the display checkpoint and its tests.
pub fn checkpoint_ownership(current: &str, before: &str, ours: Option<&str>) -> Ownership {
    if current == before {
        Ownership::Unchanged
    } else if ours == Some(current) {
        Ownership::Ours
    } else {
        Ownership::External
    }
}

/// The requested profile state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub firmware_mode: u8,
    pub energy: [u8; 16],
    pub brightness: Option<u8>,
    pub keyboard_timeout: Option<u32>,
    pub refresh_hz: Option<u32>,
}

impl Plan {
    /// Ordered steps; refresh rate stays last so earlier failures never touch
    /// the display.
    pub fn steps(&self) -> Vec<(Field, Value)> {
        let mut steps = vec![
            (Field::Firmware, Value::Mode(self.firmware_mode)),
            (Field::Energy, Value::Energy(self.energy)),
        ];
        if let Some(value) = self.brightness {
            steps.push((Field::Brightness, Value::Brightness(value)));
        }
        if let Some(value) = self.keyboard_timeout {
            steps.push((Field::KeyboardTimeout, Value::Timer(value)));
        }
        if let Some(value) = self.refresh_hz {
            steps.push((Field::RefreshRate, Value::Refresh(value)));
        }
        steps
    }
}

/// Evidence collected by one application attempt. `applied` and `restored` only
/// list fields whose new value was confirmed by an independent readback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub applied: Vec<&'static str>,
    pub failed: Vec<String>,
    pub rollback_failed: Vec<String>,
    /// Fields written by this attempt, then reverted and verified back to the
    /// original value.
    pub restored: Vec<&'static str>,
    /// Fields still holding the value this attempt wrote but confirmed to have
    /// been changed outside the transaction. Not overwritten.
    pub relinquished: Vec<String>,
    /// Fields whose readback failed, so neither success nor recovery is claimed.
    pub unverified: Vec<String>,
    /// True when a failure happened and every touched field was read back and
    /// confirmed at its original value.
    pub reverted: bool,
}

impl Outcome {
    pub fn succeeded(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Run the plan against `backend`. `Err` means preflight found a field that
/// could not be read, so no hardware write was attempted at all.
pub fn execute<B: Backend>(backend: &mut B, plan: &Plan) -> Result<Outcome, String> {
    let steps = plan.steps();
    let mut originals = Vec::with_capacity(steps.len());
    let mut checkpoints = Vec::with_capacity(steps.len());
    for (field, _) in &steps {
        let value = backend
            .read(*field)
            .map_err(|error| format!("preflight {}: {error}", field.name()))?;
        originals.push(value);
        checkpoints.push(backend.checkpoint(*field)?);
    }

    let mut outcome = Outcome::default();
    let mut failed_at = None;
    for (index, (field, target)) in steps.iter().enumerate() {
        let result = backend.write(*field, target).and_then(|()| {
            let actual = backend.read(*field)?;
            if actual == *target {
                Ok(())
            } else {
                Err(format!("readback mismatch (expected {target}, got {actual})"))
            }
        });
        match result {
            Ok(()) => outcome.applied.push(field.name()),
            Err(error) => {
                outcome.failed.push(format!("{}: {error}", field.name()));
                failed_at = Some(index);
                break;
            }
        }
    }

    let Some(last) = failed_at else {
        return Ok(outcome);
    };
    // Walk backwards over every step this attempt touched, including the failed
    // one because a setter may mutate and still return an error.
    for index in (0..=last).rev() {
        let (field, target) = &steps[index];
        let name = field.name();
        // A dedicated checkpoint restores the exact pre-transaction state
        // (e.g. DRR/display ownership) and owns the external-change decision.
        if let Some(checkpoint) = checkpoints[index].as_ref() {
            match backend.rollback(*field, &originals[index], Some(checkpoint)) {
                Ok(()) => outcome.restored.push(name),
                Err(error) => outcome
                    .rollback_failed
                    .push(format!("{name}: could not restore {} ({error})", originals[index])),
            }
            continue;
        }
        let current = match backend.read(*field) {
            Ok(value) => value,
            Err(error) => {
                outcome.unverified.push(format!("{name}: readback failed ({error}); value not verified or restored"));
                continue;
            }
        };
        if current == originals[index] {
            continue; // never changed, or the setter failed without side effects
        }
        if current != *target {
            outcome.relinquished.push(format!(
                "{name}: now {current}, which is neither the requested {target} nor the original {}; left untouched",
                originals[index]
            ));
            continue;
        }
        match backend.rollback(*field, &originals[index], None) {
            Ok(()) => outcome.restored.push(name),
            Err(error) => outcome
                .rollback_failed
                .push(format!("{name}: could not restore {} ({error})", originals[index])),
        }
    }
    outcome.reverted = outcome.rollback_failed.is_empty()
        && outcome.relinquished.is_empty()
        && outcome.unverified.is_empty();
    Ok(outcome)
}

/// Result of an explicit, user-requested Windows energy change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManualEnergy {
    /// OS accepted the change and readback shows it.
    Confirmed,
    /// The call reported an error but the OS now holds the requested scheme.
    ChangedButUnconfirmed,
    /// The call failed and readback still shows the previous scheme.
    Rejected(u32),
    /// Readback shows a scheme that is neither the previous nor the requested one.
    Conflict,
    /// Readback failed, so the current scheme is unknown.
    Unknown(String),
}

impl ManualEnergy {
    /// Whether the legacy firmware energy linkage must be switched off. It stays
    /// on only when the OS provably kept the previous scheme.
    pub fn releases_legacy_linkage(&self) -> bool {
        !matches!(self, ManualEnergy::Rejected(_))
    }

    pub fn into_result(self) -> Result<(), String> {
        match self {
            ManualEnergy::Confirmed => Ok(()),
            ManualEnergy::ChangedButUnconfirmed => Err(
                "Windows energy call reported failure but readback shows the requested scheme; legacy firmware linkage released"
                    .into(),
            ),
            ManualEnergy::Rejected(rc) => Err(format!("Windows energy change: {rc}")),
            ManualEnergy::Conflict => Err(
                "Windows energy readback shows an externally changed scheme; it was not overwritten, legacy firmware linkage released"
                    .into(),
            ),
            ManualEnergy::Unknown(error) => Err(format!(
                "Windows energy readback failed ({error}); legacy firmware linkage released so it cannot overwrite the request"
            )),
        }
    }
}

/// Classify an explicit energy change. `before` is the scheme read before the
/// call, `actual` the independent readback afterwards.
pub fn classify_manual_energy(
    rc: u32,
    requested: [u8; 16],
    before: [u8; 16],
    actual: Result<[u8; 16], String>,
) -> ManualEnergy {
    match actual {
        Ok(now) if now == requested => {
            if rc == 0 {
                ManualEnergy::Confirmed
            } else {
                ManualEnergy::ChangedButUnconfirmed
            }
        }
        Ok(now) if now == before && rc != 0 => ManualEnergy::Rejected(rc),
        Ok(_) => ManualEnergy::Conflict,
        Err(error) => ManualEnergy::Unknown(error),
    }
}
