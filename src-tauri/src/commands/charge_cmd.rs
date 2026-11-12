//! P916F-STX battery charge-limit commands.
//!
//! The exact DMI manifest id and registry BIOSVersion are checked first. A
//! non-matching machine returns `matched=false` and never opens the PMC2
//! device. Both commands run on blocking threads because the hardware path
//! performs bounded port polling.

use crate::hal::declarative;
use crate::hal::p916f_charge::{
    self, ApplyOutcome, ChargeState, IdentityCheck, NativeChargeSnapshot,
};

#[cfg(windows)]
fn read_bios_version() -> Option<String> {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ,
    };

    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!(r"HARDWARE\DESCRIPTION\System\BIOS"),
            0,
            KEY_READ,
            &mut key,
        )
        .is_err()
        {
            return None;
        }

        let mut buffer = [0u16; 128];
        let mut size = (buffer.len() * 2) as u32;
        let mut value_type = REG_SZ;
        let result = RegQueryValueExW(
            key,
            w!("BIOSVersion"),
            None,
            Some(&mut value_type),
            Some(buffer.as_mut_ptr() as *mut u8),
            Some(&mut size),
        );
        let _ = RegCloseKey(key);

        if result.is_err() || value_type != REG_SZ {
            return None;
        }

        let length = (size as usize / 2).saturating_sub(1);
        let value = String::from_utf16_lossy(&buffer[..length])
            .trim()
            .to_string();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    }
}

#[cfg(not(windows))]
fn read_bios_version() -> Option<String> {
    None
}

fn identity_gate() -> IdentityCheck {
    let manifest_id = declarative::load_matched_manifest().map(|manifest| manifest.id);
    let bios_version = read_bios_version().unwrap_or_default();
    p916f_charge::verify_identity(manifest_id.as_deref(), &bios_version)
}

fn report_abandoned_mutex() {
    if p916f_charge::take_abandoned_mutex_seen() {
        crate::log_warn!(
            "PMC2",
            "Previous PMC2 mutex owner terminated while holding Global\\OpenRevo.P916F.PMC2; ownership was recovered and the abandoned state was checked"
        );
    }
}

/// Reads the live charge-limit state. Unmatched or unavailable machines return
/// a structured state instead of a hardware error.
#[tauri::command]
pub async fn get_native_charge_state() -> Result<ChargeState, String> {
    let state = tauri::async_runtime::spawn_blocking(|| {
        let gate = identity_gate();
        p916f_charge::query_state(&gate)
    })
    .await
    .map_err(|error| format!("PMC2 query task failed: {error}"))?;

    report_abandoned_mutex();
    Ok(state)
}

/// Applies one enabled/disabled policy transaction.
///
/// `expected` must be the exact snapshot the UI last read. If the EC state has
/// changed, the returned report has `applied=false`, no write is attempted, and
/// the UI must re-query.
#[tauri::command]
pub async fn apply_native_charge_policy(
    enabled: bool,
    threshold: u8,
    expected: NativeChargeSnapshot,
) -> Result<ApplyOutcome, String> {
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let gate = identity_gate();
        p916f_charge::apply_policy(&gate, enabled, threshold, expected)
    })
    .await
    .map_err(|error| format!("PMC2 apply task failed: {error}"))??;

    report_abandoned_mutex();
    Ok(outcome)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn snapshot(gate: &IdentityCheck) -> NativeChargeSnapshot {
        let state = p916f_charge::query_state(gate);
        assert!(
            state.matched && state.available,
            "PMC2 unavailable: {state:?}"
        );
        NativeChargeSnapshot {
            enabled: state.enabled.expect("enabled"),
            threshold1: state.threshold1.expect("T1"),
            threshold2: state.threshold2.expect("T2"),
        }
    }

    struct Restore(IdentityCheck);
    impl Drop for Restore {
        fn drop(&mut self) {
            let current = p916f_charge::query_state(&self.0);
            // Do not erase settings changed externally during this acceptance test.
            if current.available
                && current.enabled == Some(1)
                && current.threshold1 == Some(80)
                && current.threshold2 == Some(100)
            {
                let result = p916f_charge::apply_policy(
                    &self.0,
                    false,
                    100,
                    NativeChargeSnapshot {
                        enabled: 1,
                        threshold1: 80,
                        threshold2: 100,
                    },
                );
                eprintln!("emergency restore: {result:?}");
            }
        }
    }

    #[test]
    #[ignore = "changes live P916F charging; requires OPENREVO_PMC2_ALLOW_LIVE_WRITES=1"]
    fn live_production_charge_roundtrip() {
        assert_eq!(
            std::env::var("OPENREVO_PMC2_ALLOW_LIVE_WRITES").as_deref(),
            Ok("1")
        );
        // Same real DMI manifest + BIOS lookup used by production IPC, not a fabricated gate.
        let gate = identity_gate();
        assert!(gate.is_matched(), "identity rejected: {gate:?}");
        let before = snapshot(&gate);
        eprintln!("BEFORE {}", before.describe());
        assert!(
            before.is_disabled_erased(),
            "will not replace an existing user policy"
        );
        let restore = Restore(gate.clone());
        let applied = p916f_charge::apply_policy(&gate, true, 80, before).expect("apply");
        eprintln!("APPLY {applied:?}");
        assert!(applied.applied);
        let expected = NativeChargeSnapshot {
            enabled: 1,
            threshold1: 80,
            threshold2: 100,
        };
        assert_eq!(snapshot(&gate), expected);
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert_eq!(snapshot(&gate), expected);
        let restored = p916f_charge::apply_policy(&gate, false, 100, expected).expect("restore");
        eprintln!("RESTORE {restored:?}");
        assert!(restored.applied);
        assert_eq!(snapshot(&gate), before);
        drop(restore);
        eprintln!("PASS_PRODUCTION_PMC2_0_0_0_TO_1_80_100_TO_0_0_0");
    }
}
