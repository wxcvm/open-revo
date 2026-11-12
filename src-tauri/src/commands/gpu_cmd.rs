//! Thin async IPC boundary: all ADLX calls stay on their owning worker, never the GUI thread.
use crate::hal::adlx::{self, DriverGraphicsState, GfxFeature, RestoreReport, UpdateReport};

#[tauri::command]
pub async fn get_gpu_driver_graphics_state() -> Result<DriverGraphicsState, String> {
    tauri::async_runtime::spawn_blocking(adlx::read_state)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_gpu_driver_toggle(feature: String, enabled: bool) -> Result<UpdateReport, String> {
    let feature = GfxFeature::from_key(&feature)
        .ok_or_else(|| format!("unknown GPU driver feature '{feature}'"))?;
    tauri::async_runtime::spawn_blocking(move || adlx::set_toggle(feature, enabled))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn set_gpu_driver_chill_fps(min_fps: i32, max_fps: i32) -> Result<UpdateReport, String> {
    tauri::async_runtime::spawn_blocking(move || adlx::set_chill_fps(min_fps, max_fps))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn set_gpu_driver_frtc_fps(fps: i32) -> Result<UpdateReport, String> {
    tauri::async_runtime::spawn_blocking(move || adlx::set_frtc_fps(fps))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn restore_gpu_driver_session() -> Result<RestoreReport, String> {
    tauri::async_runtime::spawn_blocking(adlx::restore_session)
        .await
        .map_err(|e| e.to_string())?
}
