//! Native STX usage profiles: independently verified firmware and Windows settings.
use crate::system::{config::{self, UsageProfile}, power};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};


fn energy_guid(mode: &str) -> Result<power::GUID, String> {
    match mode {
        "efficiency" => Ok(power::GUID_OFFICE_SAVER),
        "balanced" => Ok(power::GUID_BALANCED),
        "performance" => Ok(power::GUID_TURBO_PERF),
        _ => Err("Unknown Windows energy mode".into()),
    }
}
fn read_energy() -> Result<power::GUID, String> {
    power::get_actual_overlay_scheme()
}
fn apply_energy(mode: &str) -> Result<(), String> {
    let guid = energy_guid(mode)?;
    let rc = power::set_active_overlay_scheme(&guid);
    if rc != 0 { return Err(format!("Windows energy change: {rc}")); }
    if read_energy()? != guid { return Err("Windows energy readback mismatch".into()); }
    Ok(())
}
fn require_native() -> Result<(), String> {
    if !crate::hal::xingyao::is_active() { return Err("Usage profiles are currently verified only for P916F-STX".into()); }
    if config::load_config().use_vendor_bus { return Err("Switch to native control before applying a usage profile".into()); }
    Ok(())
}
#[tauri::command]
pub fn set_windows_energy(mode: String) -> Result<(), String> {
    let _guard = crate::system::vendor_bus::CONTROL_MODE_LOCK.lock();
    apply_energy(&mode)?;
    // Explicit independent selection must not be overwritten by old firmware linkage.
    config::modify_config(|cfg| cfg.high_perf_scheme = false);
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct UsageSettings {
    pub supported: bool,
    pub profiles: Vec<UsageProfile>,
    pub auto_ac: Option<String>,
    pub auto_battery: Option<String>,
    #[serde(default)]
    pub current_energy: Option<String>,
    #[serde(default)]
    pub current_native_mode: Option<u8>,
}
#[tauri::command]
pub fn get_usage_settings() -> UsageSettings {
    let cfg = config::load_config();
    let supported = crate::hal::xingyao::is_active();
    let current_energy = read_energy().ok().and_then(|guid| ["efficiency", "balanced", "performance"]
        .into_iter().find(|name| energy_guid(name).ok() == Some(guid)).map(str::to_owned));
    UsageSettings { supported, profiles: cfg.usage_profiles, auto_ac: cfg.usage_auto_ac,
        auto_battery: cfg.usage_auto_battery, current_energy,
        current_native_mode: if supported { crate::hal::xingyao::read_mode().ok() } else { None } }
}

fn validate(settings: &UsageSettings) -> Result<(), String> {
    if settings.profiles.is_empty() || settings.profiles.len() > 20 { return Err("Expected 1..20 profiles".into()); }
    let mut ids = std::collections::HashSet::new();
    for p in &settings.profiles {
        if p.id.is_empty() || p.id.len() > 64 || !ids.insert(&p.id) || p.name.trim().is_empty() || p.name.len() > 128 {
            return Err("Invalid or duplicate profile name/id".into());
        }
        if ![2, 3].contains(&p.native_mode) { return Err("STX supports balance/performance only".into()); }
        energy_guid(&p.energy_mode)?;
        if p.brightness.is_some_and(|n| n > 100) { return Err("Brightness must be 0..100".into()); }
        if p.keyboard_timeout.is_some_and(|n| n > u16::MAX as u32) { return Err("Keyboard timeout exceeds firmware range".into()); }
        if p.refresh_hz.is_some_and(|n| n == 0 || n > 1000) { return Err("Invalid refresh rate".into()); }
    }
    for id in [&settings.auto_ac, &settings.auto_battery].into_iter().flatten() {
        if !ids.contains(id) { return Err("Automatic profile does not exist".into()); }
    }
    Ok(())
}
#[tauri::command]
pub fn save_usage_settings(settings: UsageSettings) -> Result<(), String> {
    require_native()?;
    validate(&settings)?;
    config::modify_config(|cfg| {
        cfg.usage_profiles = settings.profiles;
        cfg.usage_auto_ac = settings.auto_ac;
        cfg.usage_auto_battery = settings.auto_battery;
    });
    Ok(())
}

#[derive(Serialize, Clone)]
pub struct ApplyResult {
    applied: Vec<String>,
    failed: Vec<String>,
    rollback_failed: Vec<String>,
}
#[tauri::command]
pub fn apply_usage_profile(id: String, app: tauri::AppHandle) -> Result<ApplyResult, String> {
    let _guard = crate::system::vendor_bus::CONTROL_MODE_LOCK.lock();
    require_native()?;
    let cfg = config::load_config();
    let profile = cfg.usage_profiles.iter().find(|p| p.id == id).ok_or("Profile not found")?;
    validate(&get_usage_settings())?;
    // Preflight every requested field before changing hardware.
    let old_mode = crate::hal::xingyao::read_mode()?;
    let old_energy = read_energy()?;
    let old_brightness = profile.brightness.map(|_| crate::system::display::get_brightness()).transpose()?;
    let old_timer = profile.keyboard_timeout.map(|_| crate::hal::xingyao::read_timer()).transpose()?;
    let mut result = ApplyResult { applied: vec![], failed: vec![], rollback_failed: vec![] };
    let mut step = |name: &str, outcome: Result<(), String>| {
        match outcome { Ok(()) => result.applied.push(name.into()), Err(e) => result.failed.push(format!("{name}: {e}")) }
    };
    step("firmware", crate::hal::xingyao::set_mode(profile.native_mode));
    step("windows_energy", apply_energy(&profile.energy_mode));
    if let Some(value) = profile.brightness {
        step("brightness", crate::system::display::set_brightness(value).and_then(|_| {
            if crate::system::display::get_brightness()? == value { Ok(()) } else { Err("Brightness readback mismatch".into()) }
        }));
    }
    if let Some(value) = profile.keyboard_timeout { step("keyboard_timeout", crate::hal::xingyao::set_timer(value)); }
    // Refresh is last, so a failed earlier field never changes the display.
    if result.failed.is_empty() {
        if let Some(value) = profile.refresh_hz {
            match crate::system::display_policy::set_primary_fixed(value) {
                Ok(()) => result.applied.push("refresh_rate".into()),
                Err(e) => result.failed.push(format!("refresh_rate: {e}")),
            }
        }
    }
    if !result.failed.is_empty() {
        let mut restore = |name: &str, outcome: Result<(), String>| {
            if let Err(e) = outcome { result.rollback_failed.push(format!("{name}: {e}")); }
        };
        if crate::hal::xingyao::read_mode().ok() == Some(profile.native_mode) {
            restore("firmware", crate::hal::xingyao::set_mode(old_mode));
        }
        if read_energy().ok() == energy_guid(&profile.energy_mode).ok() {
            restore("windows_energy", if power::set_active_overlay_scheme(&old_energy) == 0 && read_energy().ok() == Some(old_energy) { Ok(()) } else { Err("Restore rejected or unconfirmed".into()) });
        }
        if let Some(value) = old_brightness {
            if crate::system::display::get_brightness().ok() == profile.brightness { restore("brightness", crate::system::display::set_brightness(value)); }
        }
        if let Some(value) = old_timer {
            if crate::hal::xingyao::read_timer().ok() == profile.keyboard_timeout { restore("keyboard_timeout", crate::hal::xingyao::set_timer(value)); }
        }
        return Ok(result);
    }
    let (_, ac, _) = super::power_cmd::get_live_power_status();
    config::modify_config(|cfg| {
        cfg.power_mode = profile.native_mode;
        if ac { cfg.power_mode_ac = profile.native_mode; } else { cfg.power_mode_battery = profile.native_mode; }
        cfg.high_perf_scheme = false;
        if let Some(value) = profile.keyboard_timeout { cfg.keyboard_sleep_timeout = value; }
    });
    let bridge = app.state::<crate::driver::ec_bridge::SharedEcBridge>();
    bridge.lock().set_power_mode_from_hardware(profile.native_mode);
    crate::update_tray_power_mode(profile.native_mode);
    let _ = app.emit("physical-mode-switched", profile.native_mode);
    Ok(result)
}

#[tauri::command]
pub async fn get_battery_health() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(crate::system::battery::read_health)
        .await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> UsageSettings { UsageSettings { supported: true, profiles: config::default_usage_profiles(), auto_ac: None, auto_battery: None, current_energy: None, current_native_mode: None } }
    #[test] fn presets_are_valid_and_do_not_take_display_ownership() {
        let s = settings(); validate(&s).unwrap();
        assert!(s.profiles.iter().all(|p| p.refresh_hz.is_none() && p.brightness.is_none() && p.keyboard_timeout.is_none()));
        assert_ne!(energy_guid("efficiency").unwrap(), energy_guid("balanced").unwrap());
    }
    #[test] fn rejects_unknown_modes_and_dangling_automation() {
        let mut s = settings(); s.profiles[0].native_mode = 1; assert!(validate(&s).is_err());
        let mut s = settings(); s.auto_ac = Some("missing".into()); assert!(validate(&s).is_err());
        let mut s = settings(); s.profiles[0].energy_mode = "invented".into(); assert!(validate(&s).is_err());
    }
}
