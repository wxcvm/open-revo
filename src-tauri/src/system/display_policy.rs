//! Explicit, temporary display ownership. Automatic power events never set a rate.
use parking_lot::Mutex;
use std::sync::LazyLock;
use windows::Win32::Devices::Display::*;

#[derive(Clone)]
struct Snapshot {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
    signature: String,
}
static OWNED: LazyLock<Mutex<Option<(Snapshot, String)>>> = LazyLock::new(|| Mutex::new(None));

/// Exact pre-transaction display state plus the ownership record that existed
/// before the transaction. `hz` alone cannot represent DRR or an earlier
/// app-owned override.
pub struct DisplayCheckpoint {
    before: Snapshot,
    owned: Option<(Snapshot, String)>,
}

pub fn capture_checkpoint() -> Result<DisplayCheckpoint, String> {
    let owned = OWNED.lock();
    let before = snapshot()?;
    let owned = owned.clone();
    Ok(DisplayCheckpoint { before, owned })
}

/// Restore the exact checkpoint and ownership record. Refuses to overwrite a
/// display changed outside this transaction, and never falls back to
/// `follow_system` (which would restore an older user-original instead).
pub fn restore_checkpoint(checkpoint: &DisplayCheckpoint) -> Result<(), String> {
    let mut owned = OWNED.lock();
    let current = snapshot()?.signature;
    match super::usage_txn::checkpoint_ownership(
        &current, &checkpoint.before.signature,
        owned.as_ref().map(|(_, signature)| signature.as_str()),
    ) {
        super::usage_txn::Ownership::Unchanged => {
            *owned = checkpoint.owned.clone();
            return Ok(());
        }
        super::usage_txn::Ownership::External => {
            return Err("Display changed outside this scene; not overwriting it".into());
        }
        super::usage_txn::Ownership::Ours => {}
    }
    restore(&checkpoint.before)?;
    if snapshot()?.signature != checkpoint.before.signature {
        return Err("Display checkpoint restore readback mismatch".into());
    }
    *owned = checkpoint.owned.clone();
    Ok(())
}

fn restore(original: &Snapshot) -> Result<(), String> {
    unsafe {
        let flags = 0x20 | 0x8000 | 0x20000;
        let rc = SetDisplayConfig(Some(&original.paths), Some(&original.modes), SET_DISPLAY_CONFIG_FLAGS(flags | 0x40));
        if rc != 0 { return Err(format!("Display restore validation failed: {rc}")); }
        let rc = SetDisplayConfig(Some(&original.paths), Some(&original.modes), SET_DISPLAY_CONFIG_FLAGS(flags | 0x80));
        if rc != 0 { return Err(format!("Display restore failed: {rc}")); }
    }
    Ok(())
}

fn snapshot() -> Result<Snapshot, String> {
    unsafe {
        let flags = QUERY_DISPLAY_CONFIG_FLAGS(0x52);
        for _ in 0..3 {
            let (mut n, mut m) = (0, 0);
            let status = GetDisplayConfigBufferSizes(flags, &mut n, &mut m);
            if status.0 != 0 { return Err(format!("Display query: {}", status.0)); }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); m as usize];
            let status = QueryDisplayConfig(flags, &mut n, paths.as_mut_ptr(), &mut m, modes.as_mut_ptr(), None);
            if status.0 == 122 { continue; }
            if status.0 != 0 { return Err(format!("Display query: {}", status.0)); }
            paths.truncate(n as usize); modes.truncate(m as usize);
            // Compare semantic members, never padding bytes in C unions.
            let mut signature = String::new();
            for p in &paths {
                let s = &p.sourceInfo; let t = &p.targetInfo;
                signature.push_str(&format!("{:?}:{}:{}:{}:{:?}:{}:{}:{:?}:{:?}:{:?}:{:?}:{:?}:{}:{}:{};",
                    s.adapterId, s.id, s.Anonymous.modeInfoIdx, s.statusFlags,
                    t.adapterId, t.id, t.Anonymous.modeInfoIdx, t.outputTechnology, t.rotation,
                    t.scaling, t.refreshRate, t.scanLineOrdering, t.targetAvailable.0, t.statusFlags, p.flags));
            }
            for mode in &modes {
                signature.push_str(&format!("{:?}:{:?}:{}:", mode.infoType, mode.adapterId, mode.id));
                match mode.infoType.0 {
                    1 => signature.push_str(&format!("{:?}", mode.Anonymous.sourceMode)),
                    2 => { let v = mode.Anonymous.targetMode.targetVideoSignalInfo;
                        signature.push_str(&format!("{}:{:?}:{:?}:{:?}:{:?}:{}:{:?}", v.pixelRate, v.hSyncFreq, v.vSyncFreq, v.activeSize, v.totalSize, v.Anonymous.videoStandard, v.scanLineOrdering));
                    },
                    3 => signature.push_str(&format!("{:?}", mode.Anonymous.desktopImageInfo)),
                    _ => return Err("Unknown display mode type; refusing ownership".into()),
                }
            }
            return Ok(Snapshot { paths, modes, signature });
        }
        Err("Display topology changed during query".into())
    }
}

pub fn set_fixed(device: &str, hz: u32) -> Result<(), String> {
    let mut owned = OWNED.lock();
    let before = snapshot()?;
    let monitors = super::display::get_all_monitors();
    let monitor = monitors.iter().find(|m| m.device_name.eq_ignore_ascii_case(device))
        .ok_or("Display disconnected")?;
    if !monitor.supported_rates.contains(&hz) { return Err("Unsupported refresh rate".into()); }
    // Validate and apply without writing the Windows display database.
    super::display::set_monitor_refresh_rate_temporary(device, hz)?;
    let after = match snapshot() {
        Ok(after) => after,
        Err(error) => {
            let rollback = restore(&before);
            return Err(format!("{error}; immediate restore: {rollback:?}"));
        }
    };
    let actual = super::display::get_all_monitors();
    if !actual.iter().any(|m| m.device_name.eq_ignore_ascii_case(device) && m.current_hz == hz) {
        let rollback = restore(&before);
        return Err(format!("Refresh rate readback mismatch; immediate restore: {rollback:?}"));
    }
    let original = match owned.take() {
        Some((original, previous)) if previous == before.signature => original,
        _ => before,
    };
    *owned = Some((original, after.signature));
    Ok(())
}

/// Scene automation must never fall back to an external primary screen.
pub fn set_internal_fixed(hz: u32) -> Result<(), String> {
    let monitors = super::display::get_all_monitors();
    let monitor = monitors.iter().find(|m| m.is_internal).ok_or("No internal display; scene refresh unchanged")?;
    set_fixed(&monitor.device_name, hz)
}

pub fn set_primary_fixed(hz: u32) -> Result<(), String> {
    let monitors = super::display::get_all_monitors();
    let monitor = monitors.iter().find(|m| m.is_internal).or_else(|| monitors.iter().find(|m| m.is_primary))
        .ok_or("No active display")?;
    set_fixed(&monitor.device_name, hz)
}

pub fn follow_system() -> Result<(), String> {
    let mut owned = OWNED.lock();
    if let Some((original, previous)) = owned.as_ref() {
        if snapshot()?.signature != *previous {
            *owned = None; // Windows/user changed something: do not overwrite it.
            return Err("显示状态已被外部修改，已放弃恢复；现在跟随 Windows".into());
        }
        restore(original)?;
        if snapshot()?.signature != original.signature { return Err("Display restore readback mismatch".into()); }
        *owned = None;
    }
    Ok(())
}

pub fn get_display_policy() -> Result<serde_json::Value, String> {
    let mut owned = OWNED.lock();
    let state = snapshot()?;
    if owned.as_ref().is_some_and(|(_, previous)| *previous != state.signature) {
        *owned = None; // Read-only reconciliation: external changes revoke ownership.
    }
    let paths: Vec<_> = state.paths.iter().map(|p| serde_json::json!({
        "target_id": p.targetInfo.id,
        "drr_enabled": p.flags & 0x10 != 0,
        "virtual_hz": if p.targetInfo.refreshRate.Denominator == 0 { None } else {
            Some(p.targetInfo.refreshRate.Numerator as f64 / p.targetInfo.refreshRate.Denominator as f64)
        },
    })).collect();
    Ok(serde_json::json!({"policy": if owned.is_some() { "fixed" } else { "follow_system" }, "paths": paths}))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "Read-only local Windows display acceptance"]
    fn live_display_read() {
        let state = super::get_display_policy().unwrap();
        println!("DISPLAY_POLICY={state}");
        assert!(!state["paths"].as_array().unwrap().is_empty());
    }

    #[test]
    #[ignore = "Temporarily switches the internal display and restores its exact snapshot"]
    fn live_display_roundtrip() {
        let before = super::snapshot().unwrap();
        struct Restore(super::Snapshot);
        impl Drop for Restore { fn drop(&mut self) { let _ = super::restore(&self.0); } }
        let guard = Restore(before);
        let monitors = super::super::display::get_all_monitors();
        let monitor = monitors.iter().find(|m| m.is_internal).expect("Internal display required");
        let hz = if monitor.supported_rates.contains(&60) { 60 } else { monitor.current_hz };
        println!("DISPLAY_RATES={:?}; ORIGINAL_RESTORE={:?}", monitor.supported_rates, super::restore(&guard.0));
        println!("DISPLAY_ROUNDTRIP {} -> {} -> {}", monitor.current_hz, hz, monitor.current_hz);
        super::set_fixed(&monitor.device_name, hz).unwrap();
        super::follow_system().unwrap();
        assert_eq!(super::snapshot().unwrap().signature, guard.0.signature);
    }

    #[test]
    #[ignore = "Temporarily exercises nested display ownership and restores the panel"]
    fn live_display_checkpoint_roundtrip() {
        let before = super::snapshot().unwrap();
        struct Restore(super::Snapshot);
        impl Drop for Restore { fn drop(&mut self) { let _ = super::restore(&self.0); } }
        let guard = Restore(before);
        let monitors = super::super::display::get_all_monitors();
        let monitor = monitors.iter().find(|m| m.is_internal).expect("Internal display required");
        assert!(monitor.supported_rates.contains(&60) && monitor.supported_rates.contains(&120));
        super::set_fixed(&monitor.device_name, 60).unwrap();
        let checkpoint = super::capture_checkpoint().unwrap();
        super::set_fixed(&monitor.device_name, 120).unwrap();
        super::restore_checkpoint(&checkpoint).unwrap();
        assert_eq!(super::snapshot().unwrap().signature, checkpoint.before.signature);
        assert_eq!(super::get_display_policy().unwrap()["policy"], "fixed");
        super::follow_system().unwrap();
        assert_eq!(super::snapshot().unwrap().signature, guard.0.signature);
        println!("DISPLAY_CHECKPOINT_ROUNDTRIP {} -> 60 -> 120 -> 60 -> original", monitor.current_hz);
    }
}
