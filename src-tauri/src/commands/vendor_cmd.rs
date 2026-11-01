// OpenRevo - Vendor Control Bus Commands (AiStone MQTT)
//
// Every command runs on blocking threads via spawn_blocking and is serialised by
// a mutex, so the WebView main thread never waits on broker I/O.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::Serialize;

use crate::system::vendor_bus::{self, BusMessage};

static LAST_LATENCY_MS: AtomicU64 = AtomicU64::new(0);
static ACTION_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[derive(Serialize)]
pub struct VendorMessage {
    pub topic: String,
    pub payload: String,
}

#[derive(Serialize)]
pub struct VendorResponse {
    pub messages: Vec<VendorMessage>,
    pub elapsed_ms: u64,
}

fn to_messages(messages: Vec<BusMessage>) -> Vec<VendorMessage> {
    messages
        .into_iter()
        .map(|message| VendorMessage {
            topic: message.topic,
            payload: message.payload,
        })
        .collect()
}

async fn run_blocking<F>(work: F) -> Result<VendorResponse, String>
where
    F: FnOnce() -> Result<Vec<BusMessage>, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = ACTION_LOCK.lock();
        let started = Instant::now();
        let result = work();
        LAST_LATENCY_MS.store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
        result.map(|messages| VendorResponse {
            messages: to_messages(messages),
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn vendor_bus_probe() -> Result<VendorResponse, String> {
    run_blocking(vendor_bus::probe).await
}

#[tauri::command]
pub async fn vendor_is_available() -> bool {
    tauri::async_runtime::spawn_blocking(vendor_bus::is_available)
        .await
        .unwrap_or(false)
}

#[tauri::command]
pub async fn vendor_ensure_service() -> bool {
    tauri::async_runtime::spawn_blocking(vendor_bus::ensure_service)
        .await
        .unwrap_or(false)
}

#[tauri::command]
pub async fn vendor_set_service_persistent(enabled: bool) -> bool {
    tauri::async_runtime::spawn_blocking(move || vendor_bus::set_service_persistent(enabled))
        .await
        .unwrap_or(false)
}

#[tauri::command]
pub fn vendor_last_latency_ms() -> u64 {
    LAST_LATENCY_MS.load(Ordering::Relaxed)
}

#[tauri::command]
pub async fn vendor_fan_boost(enabled: bool) -> Result<VendorResponse, String> {
    run_blocking(move || vendor_bus::set_fan_boost(enabled)).await
}

#[tauri::command]
pub async fn vendor_set_power_mode(mode: u8) -> Result<VendorResponse, String> {
    if !(1..=4).contains(&mode) {
        return Err("mode must be 1..=4".to_string());
    }
    run_blocking(move || vendor_bus::set_operating_mode(mode)).await
}

#[tauri::command]
pub async fn vendor_set_fan_table(
    target: String,
    duty: Vec<u8>,
    name: Option<String>,
) -> Result<VendorResponse, String> {
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
    let name = name.unwrap_or_else(|| "OpenRevo Custom".to_string());
    run_blocking(move || {
        let mut table = [0u8; 16];
        table.copy_from_slice(&duty);
        vendor_bus::set_fan_table(&name, &target, &table)
    })
    .await
}

#[tauri::command]
pub async fn vendor_apply_fan_preset(preset: String) -> Result<VendorResponse, String> {
    run_blocking(move || vendor_bus::apply_fan_preset(&preset)).await
}

#[tauri::command]
pub async fn vendor_set_kb_timer(seconds: u32) -> Result<VendorResponse, String> {
    if seconds > 86_400 {
        return Err("seconds must be <= 86400".to_string());
    }
    run_blocking(move || vendor_bus::set_kb_timer(seconds)).await
}

#[tauri::command]
pub async fn vendor_set_kb_backlight(enabled: bool) -> Result<VendorResponse, String> {
    run_blocking(move || vendor_bus::set_kb_backlight(enabled)).await
}
