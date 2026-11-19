//! Native STX usage profiles: independently verified firmware and Windows settings.
use crate::system::{config::{self, UsageProfile}, power, usage_txn::{self, Backend, Field, Plan, Value}};
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
fn guid_bytes(guid: power::GUID) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&guid.data1.to_be_bytes());
    bytes[4..6].copy_from_slice(&guid.data2.to_be_bytes());
    bytes[6..8].copy_from_slice(&guid.data3.to_be_bytes());
    bytes[8..16].copy_from_slice(&guid.data4);
    bytes
}
fn guid_from_bytes(bytes: [u8; 16]) -> power::GUID {
    power::GUID {
        data1: u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        data2: u16::from_be_bytes([bytes[4], bytes[5]]),
        data3: u16::from_be_bytes([bytes[6], bytes[7]]),
        data4: [bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]],
    }
}
fn energy_bytes(mode: &str) -> Result<[u8; 16], String> {
    Ok(guid_bytes(energy_guid(mode)?))
}
fn require_native() -> Result<(), String> {
    if !crate::hal::xingyao::is_active() { return Err("Usage profiles are currently verified only for P916F-STX".into()); }
    if config::load_config().use_vendor_bus { return Err("Switch to native control before applying a usage profile".into()); }
    Ok(())
}
#[tauri::command]
pub async fn set_windows_energy(mode: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || set_windows_energy_inner(mode)).await.map_err(|e| e.to_string())?
}
fn set_windows_energy_inner(mode: String) -> Result<(), String> {
    let _guard = crate::system::vendor_bus::CONTROL_MODE_LOCK.lock();
    // Lock order: CONTROL then ENERGY. Held across check, write, readback and
    // the legacy-linkage config release so firmware linkage cannot race it.
    let _energy = crate::system::vendor_bus::ENERGY_LOCK.lock();
    let requested = energy_bytes(&mode)?;
    let before = guid_bytes(read_energy()?);
    let rc = power::set_active_overlay_scheme(&guid_from_bytes(requested));
    // An error code does not prove the OS kept the old scheme, so read back anyway.
    let actual = read_energy().map(guid_bytes);
    let outcome = usage_txn::classify_manual_energy(rc, requested, before, actual);
    // Explicit independent selection must not be overwritten by old firmware linkage.
    if outcome.releases_legacy_linkage() {
        config::try_modify_config(|cfg| { cfg.high_perf_scheme = false; Ok(()) })
            .map_err(|error| format!("系统能源模式已回读，但未能保存联动设置：{error}"))?;
    }
    outcome.into_result()
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

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageDraft {
    profiles: Vec<UsageProfile>,
    auto_ac: Option<String>,
    auto_battery: Option<String>,
}
impl UsageDraft {
    fn matches(&self, cfg: &config::AppConfig) -> bool {
        self.profiles == cfg.usage_profiles && self.auto_ac == cfg.usage_auto_ac &&
            self.auto_battery == cfg.usage_auto_battery
    }
}
#[tauri::command]
pub async fn get_usage_settings() -> Result<UsageSettings, String> {
    tauri::async_runtime::spawn_blocking(read_usage_settings).await.map_err(|e| e.to_string())
}
fn read_usage_settings() -> UsageSettings {
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
pub async fn save_usage_settings(settings: UsageSettings, expected: UsageDraft) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || persist_usage_settings(settings, expected))
        .await.map_err(|e| e.to_string())?
}
fn persist_usage_settings(settings: UsageSettings, expected: UsageDraft) -> Result<(), String> {
    let _control = crate::system::vendor_bus::CONTROL_MODE_LOCK.lock();
    let _energy = crate::system::vendor_bus::ENERGY_LOCK.lock();
    require_native()?;
    validate(&settings)?;
    config::try_modify_config(|cfg| {
        if !expected.matches(cfg) {
            return Err("已保存场景在别处发生改变。草稿未覆盖现有配置；请重新读取后再编辑。".into());
        }
        cfg.usage_profiles = settings.profiles;
        cfg.usage_auto_ac = settings.auto_ac;
        cfg.usage_auto_battery = settings.auto_battery;
        if cfg.has_automatic_usage_profile() { cfg.high_perf_scheme = false; }
        Ok(())
    })
}

#[derive(Serialize, Clone)]
pub struct ApplyResult {
    applied: Vec<String>,
    failed: Vec<String>,
    rollback_failed: Vec<String>,
    #[serde(default)]
    restored: Vec<String>,
    #[serde(default)]
    relinquished: Vec<String>,
    #[serde(default)]
    unverified: Vec<String>,
    /// Every touched field was read back and confirmed at its original value.
    #[serde(default)]
    reverted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    persistence_error: Option<String>,
}
impl From<usage_txn::Outcome> for ApplyResult {
    fn from(outcome: usage_txn::Outcome) -> Self {
        ApplyResult {
            applied: outcome.applied.into_iter().map(str::to_owned).collect(),
            failed: outcome.failed,
            rollback_failed: outcome.rollback_failed,
            restored: outcome.restored.into_iter().map(str::to_owned).collect(),
            relinquished: outcome.relinquished,
            unverified: outcome.unverified,
            reverted: outcome.reverted,
            persistence_error: None,
        }
    }
}

/// Hardware adapter for the pure transaction engine.
struct SystemBackend;
fn read_internal_refresh() -> Result<u32, String> {
    let monitors = crate::system::display::get_all_monitors();
    monitors.iter().find(|m| m.is_internal)
        .map(|m| m.current_hz).ok_or_else(|| "No active display".into())
}
impl Backend for SystemBackend {
    type Checkpoint = crate::system::display_policy::DisplayCheckpoint;
    fn checkpoint(&mut self, field: Field) -> Result<Option<Self::Checkpoint>, String> {
        match field {
            // Hz alone cannot describe DRR or an earlier app-owned override.
            Field::RefreshRate => crate::system::display_policy::capture_checkpoint().map(Some),
            _ => Ok(None),
        }
    }
    fn rollback(&mut self, field: Field, original: &Value, checkpoint: Option<&Self::Checkpoint>) -> Result<(), String> {
        match (field, checkpoint) {
            (Field::RefreshRate, Some(checkpoint)) => crate::system::display_policy::restore_checkpoint(checkpoint),
            _ => {
                self.write(field, original)?;
                let actual = self.read(field)?;
                if actual == *original { Ok(()) } else {
                    Err(format!("restore readback mismatch (expected {original}, got {actual})"))
                }
            }
        }
    }
    fn read(&mut self, field: Field) -> Result<Value, String> {
        match field {
            Field::Firmware => crate::hal::xingyao::read_mode().map(Value::Mode),
            Field::Energy => read_energy().map(|guid| Value::Energy(guid_bytes(guid))),
            Field::Brightness => crate::system::display::get_brightness().map(Value::Brightness),
            Field::KeyboardTimeout => crate::hal::xingyao::read_timer().map(Value::Timer),
            Field::RefreshRate => read_internal_refresh().map(Value::Refresh),
        }
    }
    fn write(&mut self, field: Field, value: &Value) -> Result<(), String> {
        match (field, value) {
            (Field::Firmware, Value::Mode(mode)) => crate::hal::xingyao::set_mode(*mode),
            (Field::Energy, Value::Energy(bytes)) => {
                let rc = power::set_active_overlay_scheme(&guid_from_bytes(*bytes));
                if rc == 0 { Ok(()) } else { Err(format!("Windows energy change: {rc}")) }
            }
            (Field::Brightness, Value::Brightness(level)) => crate::system::display::set_brightness(*level),
            (Field::KeyboardTimeout, Value::Timer(seconds)) => crate::hal::xingyao::set_timer(*seconds),
            (Field::RefreshRate, Value::Refresh(hz)) => crate::system::display_policy::set_internal_fixed(*hz),
            _ => Err("Profile value does not match its setting".into()),
        }
    }
}

#[tauri::command]
pub async fn apply_usage_profile(id: String, app: tauri::AppHandle) -> Result<ApplyResult, String> {
    tauri::async_runtime::spawn_blocking(move || apply_profile(Some(id), None, app)?.ok_or_else(|| "未选择场景".into()))
        .await.map_err(|e| e.to_string())?
}

/// Power events apply a scene without rewriting the user's manual AC/DC defaults.
pub fn apply_automatic_profile(on_ac: bool, app: tauri::AppHandle) -> Result<Option<ApplyResult>, String> {
    apply_profile(None, Some(on_ac), app)
}

fn apply_profile(id: Option<String>, automatic: Option<bool>, app: tauri::AppHandle) -> Result<Option<ApplyResult>, String> {
    let _guard = crate::system::vendor_bus::CONTROL_MODE_LOCK.lock();
    // Lock order: CONTROL then ENERGY. Held over the whole transaction and the
    // config save so legacy firmware linkage cannot write between steps.
    let _energy = crate::system::vendor_bus::ENERGY_LOCK.lock();
    let cfg = config::load_config();
    let save_manual_preferences = automatic.is_none();
    let id = match automatic {
        Some(true) => cfg.usage_auto_ac.clone(),
        Some(false) => cfg.usage_auto_battery.clone(),
        None => id,
    };
    let Some(id) = id else { return Ok(None); };
    require_native()?;
    let settings = UsageSettings { supported: true, profiles: cfg.usage_profiles, auto_ac: cfg.usage_auto_ac,
        auto_battery: cfg.usage_auto_battery, current_energy: None, current_native_mode: None };
    validate(&settings)?;
    let profile = settings.profiles.iter().find(|p| p.id == id).ok_or("Profile not found")?;
    let plan = Plan {
        firmware_mode: profile.native_mode,
        energy: energy_bytes(&profile.energy_mode)?,
        brightness: profile.brightness,
        keyboard_timeout: profile.keyboard_timeout,
        refresh_hz: profile.refresh_hz,
    };
    // The engine preflights every field, stops at the first error and only
    // reverts values it can prove still belong to this attempt.
    let outcome = usage_txn::execute(&mut SystemBackend, &plan)?;
    if !outcome.succeeded() { return Ok(Some(ApplyResult::from(outcome))); }
    let (_, ac, _) = super::power_cmd::get_live_power_status();
    let persisted = config::try_modify_config(|cfg| {
        cfg.power_mode = profile.native_mode;
        if save_manual_preferences {
            if ac { cfg.power_mode_ac = profile.native_mode; } else { cfg.power_mode_battery = profile.native_mode; }
        }
        cfg.high_perf_scheme = false;
        if save_manual_preferences {
            if let Some(value) = profile.keyboard_timeout { cfg.keyboard_sleep_timeout = value; }
        }
        Ok(())
    });
    let bridge = app.state::<crate::driver::ec_bridge::SharedEcBridge>();
    bridge.lock().set_power_mode_from_hardware(profile.native_mode);
    crate::update_tray_power_mode(profile.native_mode);
    let _ = app.emit("physical-mode-switched", profile.native_mode);
    let mut report = ApplyResult::from(outcome);
    if let Err(error) = persisted {
        report.persistence_error = Some(error);
    }
    Ok(Some(report))
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
    #[test] fn energy_guid_survives_the_transaction_byte_encoding() {
        for mode in ["efficiency", "balanced", "performance"] {
            let guid = energy_guid(mode).unwrap();
            assert_eq!(guid_from_bytes(guid_bytes(guid)), guid);
        }
        assert_ne!(guid_bytes(power::GUID_OFFICE_SAVER), guid_bytes(power::GUID_BALANCED));
    }
    #[test]
    #[ignore = "Temporarily changes native firmware/Windows energy, restoring both; no config saves"]
    fn live_native_scene_roundtrip() {
        require_native().unwrap();
        let _control = crate::system::vendor_bus::CONTROL_MODE_LOCK.lock();
        let _energy = crate::system::vendor_bus::ENERGY_LOCK.lock();
        let before_mode = crate::hal::xingyao::read_mode().unwrap();
        let before_energy = read_energy().unwrap();
        struct Restore(u8, power::GUID);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = crate::hal::xingyao::set_mode(self.0);
                power::set_active_overlay_scheme(&self.1);
            }
        }
        let _restore = Restore(before_mode, before_energy);
        let plan = |mode, energy| Plan { firmware_mode: mode, energy,
            brightness: None, keyboard_timeout: None, refresh_hz: None };
        let result = usage_txn::execute(&mut SystemBackend, &plan(2, energy_bytes("balanced").unwrap())).unwrap();
        assert!(result.succeeded(), "{result:?}");
        assert_eq!(crate::hal::xingyao::read_mode().unwrap(), 2);
        assert_eq!(read_energy().unwrap(), power::GUID_BALANCED);
        let result = usage_txn::execute(&mut SystemBackend, &plan(before_mode, guid_bytes(before_energy))).unwrap();
        assert!(result.succeeded(), "{result:?}");
        assert_eq!(crate::hal::xingyao::read_mode().unwrap(), before_mode);
        assert_eq!(read_energy().unwrap(), before_energy);
        println!("NATIVE_SCENE_ROUNDTRIP mode {before_mode} -> 2 -> {before_mode}; energy restored; config untouched");
    }
}
