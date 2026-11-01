// OpenRevo - Vendor Control Bus Commands (AiStone MQTT)
// Exposes the vendor service bus (localhost:13688) so fan curves / boost can be
// driven on models whose WMI mailbox has no write channel.

use serde::Serialize;

use crate::system::vendor_bus::{self, BusMessage};

#[derive(Serialize)]
pub struct VendorMessage {
    pub topic: String,
    pub payload: String,
}

fn to_json(messages: Vec<BusMessage>) -> Vec<VendorMessage> {
    messages
        .into_iter()
        .map(|message| VendorMessage {
            topic: message.topic,
            payload: message.payload,
        })
        .collect()
}

#[tauri::command]
pub fn vendor_ensure_service() -> bool {
    vendor_bus::ensure_service()
}

#[tauri::command]
pub fn vendor_bus_probe() -> Result<Vec<VendorMessage>, String> {
    vendor_bus::probe().map(to_json)
}

#[tauri::command]
pub fn vendor_fan_boost(enabled: bool) -> Result<Vec<VendorMessage>, String> {
    vendor_bus::set_fan_boost(enabled).map(to_json)
}

#[tauri::command]
pub fn vendor_set_power_mode(mode: u8) -> Result<Vec<VendorMessage>, String> {
    if !(1..=4).contains(&mode) {
        return Err("mode must be 1..=4".to_string());
    }
    vendor_bus::set_operating_mode(mode).map(to_json)
}

#[tauri::command]
pub fn vendor_set_fan_table(
    target: String,
    duty: Vec<u8>,
    name: Option<String>,
) -> Result<Vec<VendorMessage>, String> {
    let target = target.to_ascii_uppercase();
    if target != "CPU" && target != "GPU" {
        return Err("target must be CPU or GPU".to_string());
    }
    if duty.len() != 16 {
        return Err("duty must contain exactly 16 points".to_string());
    }
    if duty.iter().any(|value| *value > 100) {
        return Err("duty values must be 0..=100".to_string());
    }
    let mut table = [0u8; 16];
    table.copy_from_slice(&duty);
    let table_name = name.unwrap_or_else(|| "OpenRevo Custom".to_string());
    vendor_bus::set_fan_table(&table_name, &target, &table).map(to_json)
}
