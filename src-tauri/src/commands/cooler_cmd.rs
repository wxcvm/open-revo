// OpenRevo - Water Cooler BLE Commands
// Handles BLE water cooler telemetry, pump/fan speeds, curves, pairing, filling, and LED control.

use crate::driver::ble_cooler::CoolerTelemetry;
use crate::system::config::CoolerCurvePoint;

#[tauri::command]
pub fn start_water_cooler(pump_v: Option<u8>, fan_d: Option<u8>) -> Result<CoolerTelemetry, String> {
    if crate::hal::xingyao::is_active() { return Err("本机未适配外置水冷控制".into()); }
    let cooler = crate::driver::ble_cooler::get_global_cooler();
    if !cooler.get_telemetry().connected {
        if let Some(paired) = crate::driver::ble_cooler::find_paired_water_cooler_mac() {
            cooler.set_mac(paired);
        }
    }
    cooler.resume_after_wake();
    let is_custom = {
        let strat = cooler.state().strategy.read().clone();
        strat == "custom"
    };
    if is_custom && (pump_v.is_some() || fan_d.is_some()) {
        cooler.set_params(pump_v, fan_d);
    }
    cooler.clone().start_active_cooling();
    Ok(cooler.get_telemetry())
}

#[tauri::command]
pub fn get_water_cooler_status() -> CoolerTelemetry {
    crate::driver::ble_cooler::get_global_cooler().get_telemetry()
}

#[tauri::command]
pub fn set_water_cooler_strategy(
    strategy: String,
    curve: Option<Vec<CoolerCurvePoint>>,
) -> CoolerTelemetry {
    let cooler = crate::driver::ble_cooler::get_global_cooler();
    cooler.set_strategy(&strategy, curve);
    cooler.get_telemetry()
}

#[tauri::command]
pub fn set_water_cooler_pump_thresholds(start_temp: u8, turbo_temp: u8) -> CoolerTelemetry {
    crate::log_info!("COOLER_CMD", "[IPC] set_water_cooler_pump_thresholds: start_temp={}, turbo_temp={}", start_temp, turbo_temp);
    let cooler = crate::driver::ble_cooler::get_global_cooler();
    cooler.set_pump_thresholds(start_temp, turbo_temp)
}

#[tauri::command]
pub fn set_water_cooler_speed(pump_v: Option<u8>, fan_d: Option<u8>) -> CoolerTelemetry {
    crate::log_warn!("COOLER_CMD", "[IPC] set_water_cooler_speed received: pump_v={:?}, fan_d={:?}", pump_v, fan_d);
    let cooler = crate::driver::ble_cooler::get_global_cooler();
    *cooler.state().strategy.write() = "custom".to_string();
    cooler.set_params(pump_v, fan_d);
    let tel = cooler.get_telemetry();
    crate::log_warn!("COOLER_CMD", "[IPC] Resulting telemetry -> pump_tier={}, pump_voltage={}, fan_duty={}", tel.pump_tier, tel.pump_voltage, tel.fan_duty);
    tel
}

#[tauri::command]
pub fn set_water_cooler_led(on: bool, r: u8, g: u8, b: u8, mode: u8) -> CoolerTelemetry {
    let cooler = crate::driver::ble_cooler::get_global_cooler();
    cooler.set_led(on, r, g, b, mode);
    cooler.get_telemetry()
}

#[tauri::command]
pub fn reconnect_water_cooler() -> CoolerTelemetry {
    let cooler = crate::driver::ble_cooler::get_global_cooler();
    if !cooler.get_telemetry().connected {
        if let Some(paired) = crate::driver::ble_cooler::find_paired_water_cooler_mac() {
            cooler.set_mac(paired);
        }
    }
    cooler.resume_after_wake();
    cooler.reconnect();
    cooler.clone().start_active_cooling();
    cooler.get_telemetry()
}

#[tauri::command]
pub fn pair_water_cooler() -> Result<CoolerTelemetry, String> {
    let cooler = crate::driver::ble_cooler::get_global_cooler();
    cooler.stop_and_shutdown();
    std::thread::sleep(std::time::Duration::from_millis(300));

    // First check Windows PnP paired device (instant 0ms)
    let new_mac = if let Some(paired) = crate::driver::ble_cooler::find_paired_water_cooler_mac() {
        crate::log_warn!("BLE_COOLER", "Pairing: Found paired water cooler from Windows PnP: {:012X}", paired);
        paired
    } else {
        crate::driver::ble_cooler::scan_for_water_cooler(std::time::Duration::from_secs(8))?
    };
    cooler.set_mac(new_mac);
    cooler.resume_after_wake();
    cooler.clone().start_active_cooling();
    Ok(cooler.get_telemetry())
}

#[tauri::command]
pub fn fill_water_cooler() -> Result<(), String> {
    let cooler = crate::driver::ble_cooler::get_global_cooler();
    cooler.trigger_fill_water();
    Ok(())
}

#[tauri::command]
pub fn reset_water_cooler() -> CoolerTelemetry {
    let cooler = crate::driver::ble_cooler::get_global_cooler();
    cooler.reset_defaults();
    cooler.get_telemetry()
}

#[tauri::command]
pub fn stop_water_cooler() -> CoolerTelemetry {
    let cooler = crate::driver::ble_cooler::get_global_cooler();
    cooler.stop_and_shutdown();
    cooler.get_telemetry()
}

#[tauri::command]
pub fn get_water_cooler_enabled() -> bool {
    crate::system::config::is_water_cooler_enabled()
}

#[tauri::command]
pub fn set_water_cooler_enabled(enabled: bool) -> Result<bool, String> {
    if enabled && crate::hal::xingyao::is_active() {
        return Err("本机未适配外置水冷控制".into());
    }
    let mut cfg = crate::system::config::load_config();
    cfg.water_cooler_enabled = Some(enabled);
    cfg.module_toggles.insert("water".to_string(), enabled);
    crate::system::config::save_config(&cfg);

    if enabled {
        let cooler = crate::driver::ble_cooler::get_global_cooler();
        if !cooler.get_telemetry().connected {
            if let Some(paired) = crate::driver::ble_cooler::find_paired_water_cooler_mac() {
                cooler.set_mac(paired);
            }
        }
        cooler.resume_after_wake();
        cooler.start_active_cooling();
        crate::log_info!("BLE_COOLER", "Water cooler module forcefully enabled by user; active cooling supervisor started.");
    } else {
        let cooler = crate::driver::ble_cooler::get_global_cooler();
        cooler.stop_and_shutdown();
        crate::log_info!("BLE_COOLER", "Water cooler module disabled by user; cooler shutdown and BLE disconnect triggered.");
    }
    Ok(enabled)
}

#[tauri::command]
pub fn scan_water_cooler_devices() -> Result<Vec<crate::driver::ble_cooler::WaterCoolerDevice>, String> {
    crate::driver::ble_cooler::scan_all_water_coolers(std::time::Duration::from_secs(3))
}

#[tauri::command]
pub fn select_water_cooler(mac: String) -> Result<CoolerTelemetry, String> {
    let cooler = crate::driver::ble_cooler::get_global_cooler();
    let mac_u64 = crate::driver::ble_cooler::mac_str_to_u64(&mac)
        .ok_or_else(|| "Invalid MAC address format".to_string())?;

    cooler.stop_and_shutdown();
    std::thread::sleep(std::time::Duration::from_millis(250));

    cooler.set_mac(mac_u64);
    cooler.resume_after_wake();
    cooler.clone().start_active_cooling();

    crate::log_info!("BLE_COOLER", "User manually selected and locked water cooler MAC: {}", mac);
    Ok(cooler.get_telemetry())
}

