//! Live ADLX tests against the installed AMD driver.
//!
//! Every test here is `#[ignore]`: the default `cargo test` run never touches the driver. Run them
//! explicitly with:
//!
//!   cargo test --test adlx_driver_live -- --ignored --nocapture
//!
//! The read-only tests are safe. The single opt-in round-trip test additionally requires
//! `OPENREVO_ADLX_ALLOW_LIVE_WRITES=1`, because it changes a real driver setting and restores it.

use open_revo_lib::hal::adlx::{self, GfxFeature, GfxField, Ledger};

#[test]
#[ignore = "reads the real AMD driver; run with --ignored"]
fn live_adlx_loads_from_system32_and_reports_version() {
    let path = adlx::system_adlx_dll_path().expect("system ADLX path");
    assert!(
        path.to_ascii_lowercase()
            .ends_with("\\system32\\amdadlx64.dll"),
        "unexpected DLL path: {path}"
    );
    assert!(
        std::path::Path::new(&path).exists(),
        "amdadlx64.dll missing at {path}; AMD driver not installed?"
    );

    let state = adlx::read_state();
    assert!(state.available, "ADLX unavailable: {:?}", state.detail);
    let version = state.adlx_version.expect("ADLX version");
    assert!(
        version.starts_with("1."),
        "expected ADLX 1.x, got {version}"
    );
    assert!(state.gpu_name.is_some(), "GPU name missing");

    // Never claim a state we did not read.
    for (label, toggle) in [
        ("anti_lag", &state.anti_lag),
        ("image_sharpening", &state.image_sharpening),
        ("enhanced_sync", &state.enhanced_sync),
    ] {
        if !toggle.supported {
            assert!(
                toggle.enabled.is_none(),
                "{label}: unsupported must not report an enabled state"
            );
        }
    }
    assert!(
        state.chill.supported,
        "Chill is expected to be supported on this GPU"
    );
}

/// Documents the validated capability matrix of this machine (880M / ADLX 1.5.0.124) as a live
/// assertion. If a driver update changes it, this test is the place that says so.
#[test]
#[ignore = "reads the real AMD driver; run with --ignored"]
fn live_adlx_capability_matrix_matches_the_validated_probe() {
    let state = adlx::read_state();
    if !state.available {
        eprintln!("skipping: ADLX unavailable ({:?})", state.detail);
        return;
    }
    // ABI evidence: the full readback produced by these bindings on the installed driver.
    eprintln!(
        "adlx evidence: {}",
        serde_json::to_string_pretty(&state).unwrap_or_else(|e| format!("<serialize failed: {e}>"))
    );
    assert!(state.anti_lag.supported);
    assert!(state.chill.supported);
    assert!(state.image_sharpening.supported);
    assert!(state.frame_rate_target.supported);
    // EnhancedSync: unsupported by this driver/GPU.
    assert!(!state.enhanced_sync.supported);

    // Manual tuning paths are unsupported -> no clock/voltage/fan/power controls exist.
    assert_eq!(
        state.tuning.manual_gfx,
        Some(false),
        "manual GFX tuning unexpectedly supported"
    );
    assert_eq!(
        state.tuning.manual_vram,
        Some(false),
        "manual VRAM tuning unexpectedly supported"
    );
    assert_eq!(
        state.tuning.manual_fan,
        Some(false),
        "manual fan tuning unexpectedly supported"
    );
    assert_eq!(
        state.tuning.manual_power,
        Some(false),
        "manual power tuning unexpectedly supported"
    );

    // Driver-reported FPS ranges must be sane whenever a read succeeded.
    if let Some(range) = state.chill.range {
        assert!(range.max > range.min);
        assert!(range.step > 0);
    }
    if let Some(range) = state.frame_rate_target.range {
        assert!(range.max > range.min);
        assert!(range.step > 0);
    }
}

#[test]
#[ignore = "reads the real AMD driver; run with --ignored"]
fn live_adlx_ledger_starts_empty_and_rest_does_not_write() {
    // The ledger only records writes, so a fresh process has nothing to restore.
    assert_eq!(adlx::changed_fields(), Vec::<String>::new());
    let report = adlx::restore_session().expect("restore call");
    assert!(report.restored.is_empty());
    assert!(
        report.message.unwrap_or_default().contains("no settings"),
        "restore must not touch the driver when nothing was changed"
    );
    // A fresh ledger can never claim it has something to restore.
    assert!(!Ledger::new().can_restore());
    assert_eq!(GfxField::ChillFps.key(), "chill_fps");
}

/// Opt-in, ignored by default, and additionally gated on an explicit environment variable: it
/// performs a real driver write and restores the previous value afterwards.
#[test]
#[ignore = "writes a real AMD driver setting; requires OPENREVO_ADLX_ALLOW_LIVE_WRITES=1"]
fn live_adlx_toggle_round_trip_is_verified_and_restored() {
    if std::env::var("OPENREVO_ADLX_ALLOW_LIVE_WRITES").as_deref() != Ok("1") {
        eprintln!("skipping live write test: set OPENREVO_ADLX_ALLOW_LIVE_WRITES=1 to opt in");
        return;
    }
    let before = adlx::read_state();
    assert!(before.available, "ADLX unavailable: {:?}", before.detail);
    assert!(before.image_sharpening.supported);
    let original = before
        .image_sharpening
        .enabled
        .expect("read sharpening state");
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            if self.0 {
                let _ = adlx::restore_session();
            }
        }
    }
    let mut guard = Restore(true);
    let report = adlx::set_toggle(GfxFeature::ImageSharpening, !original).expect("set sharpening");
    assert!(report.applied, "write not confirmed: {report:?}");
    let changed = adlx::read_state();
    assert_eq!(changed.image_sharpening.enabled, Some(!original));
    assert_eq!(changed.chill, before.chill, "must preserve Chill");
    assert_eq!(changed.anti_lag, before.anti_lag);
    assert_eq!(changed.frame_rate_target, before.frame_rate_target);
    let restore = adlx::restore_session().expect("restore session");
    assert!(
        restore.restored.iter().any(|f| f == "image_sharpening"),
        "{restore:?}"
    );
    let after = adlx::read_state();
    assert_eq!(after.image_sharpening, before.image_sharpening);
    assert_eq!(after.chill, before.chill);
    assert_eq!(after.anti_lag, before.anti_lag);
    assert_eq!(after.frame_rate_target, before.frame_rate_target);
    guard.0 = false;
    println!(
        "SHARPENING_ROUNDTRIP {original} -> {} -> {original}; Chill and other states preserved",
        !original
    );
}

#[test]
#[ignore = "writes real GPU settings and restores them; requires OPENREVO_ADLX_ALLOW_LIVE_WRITES=1"]
fn live_adlx_fps_and_mutual_exclusion_roundtrip() {
    assert_eq!(
        std::env::var("OPENREVO_ADLX_ALLOW_LIVE_WRITES").as_deref(),
        Ok("1")
    );
    let before = adlx::read_state();
    assert!(before.available);
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = adlx::restore_session();
        }
    }
    let _guard = Restore;
    let min = before.chill.min_fps.unwrap();
    let max = before.chill.max_fps.unwrap();
    let range = before.chill.range.unwrap();
    let new_min = if min > range.min {
        min - range.step
    } else {
        min
    };
    let new_max = if max < range.max {
        max + range.step
    } else {
        max
    };
    let report = adlx::set_chill_fps(new_min, new_max).unwrap();
    assert!(report.applied, "{report:?}");
    let read = adlx::read_state();
    assert_eq!(
        (read.chill.min_fps, read.chill.max_fps),
        (Some(new_min), Some(new_max))
    );
    let fps = before.frame_rate_target.fps.unwrap();
    let range = before.frame_rate_target.range.unwrap();
    let new_fps = if fps < range.max {
        fps + range.step
    } else {
        fps - range.step
    };
    let enabled = adlx::set_toggle(GfxFeature::FrameRateTarget, true).unwrap();
    println!("FRTC_ENABLE: {enabled:?}");
    let report = adlx::set_frtc_fps(new_fps).unwrap();
    println!("FRTC_FPS: {report:?}");
    assert!(enabled.applied && report.applied, "FRTC write refused");
    assert_eq!(adlx::read_state().frame_rate_target.fps, Some(new_fps));
    let report = adlx::set_toggle(GfxFeature::AntiLag, !before.anti_lag.enabled.unwrap()).unwrap();
    assert!(report.applied, "{report:?}");
    if report.state.anti_lag.enabled == Some(true) {
        assert_eq!(report.state.chill.enabled, Some(false));
    }
    let restored = adlx::restore_session().unwrap();
    assert!(restored.skipped.is_empty(), "{restored:?}");
    let after = adlx::read_state();
    assert_eq!(after.anti_lag, before.anti_lag);
    assert_eq!(after.chill, before.chill);
    assert_eq!(after.frame_rate_target, before.frame_rate_target);
    assert_eq!(after.image_sharpening, before.image_sharpening);
    println!("GPU_FPS_ROUNDTRIP Chill {min}/{max} -> {new_min}/{new_max} -> {min}/{max}; FRTC {fps} -> {new_fps} -> {fps}; AntiLag/Chill mutual exclusion restored");
}

#[test]
#[ignore = "read-only real ADLX worker shutdown acceptance; run individually"]
fn live_worker_shutdown_on_owning_thread() {
    assert!(adlx::read_state().available);
    adlx::service::shutdown();
    println!("ADLX worker released interfaces and terminated on owning thread");
}

#[test]
#[ignore = "reads real AMD GPU metrics; no tuning writes"]
fn live_gpu_metric_cache_fills_without_blocking_heartbeat() {
    let name = adlx::read_state().gpu_name.unwrap();
    let mut found = None;
    for _ in 0..30 {
        found = adlx::service::cached_metrics(&name);
        if found.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let sample = found.expect("fresh matching ADLX metrics");
    println!("GPU_METRICS={}", serde_json::to_string(&sample).unwrap());
    assert!(sample.temperature_c.is_some());
    assert!(sample.core_mhz.is_some());
    assert!(sample.memory_mhz.is_some());
    assert!(adlx::service::cached_metrics("not the same adapter").is_none());
    adlx::service::shutdown();
}
