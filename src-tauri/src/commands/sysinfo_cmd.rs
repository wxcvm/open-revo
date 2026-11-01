// OpenRevo - System Monitor Commands
// Exposes RAM / volume capacity telemetry to the 系统监控 dashboard.

use crate::system::sysinfo::{self, SystemStats};

#[tauri::command]
pub fn get_system_stats() -> SystemStats {
    sysinfo::collect()
}
