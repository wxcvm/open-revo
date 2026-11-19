// OpenRevo - System Monitor Commands
// Exposes RAM / volume capacity telemetry to the 系统监控 dashboard.

use crate::system::sysinfo::{self, SystemStats};

#[tauri::command]
pub async fn get_system_stats() -> Result<SystemStats, String> {
    tauri::async_runtime::spawn_blocking(sysinfo::collect).await.map_err(|e| e.to_string())
}

const TRACE_HEADER: &str = "timestamp_utc,gap_before,cpu_usage_pct,gpu_usage_pct,cpu_temp_c,gpu_temp_c,cpu_clock_mhz,gpu_clock_mhz,main_fan_rpm,aux_fan_rpm";

fn validate_csv(csv: &str) -> Result<(), String> {
    if csv.len() > 128 * 1024 { return Err("导出内容过大".into()); }
    let mut lines = csv.lines();
    if lines.next() != Some(TRACE_HEADER) { return Err("不是性能记录 CSV".into()); }
    let mut count = 0;
    for row in lines {
        count += 1;
        let fields = row.split(',').collect::<Vec<_>>();
        if count > 600 || fields.len() != 10 || !matches!(fields[1], "0" | "1") ||
            fields[0].len() != 24 || !fields[0].ends_with('Z') ||
            !fields[0].bytes().all(|b| b.is_ascii_digit() || b"-T:.Z".contains(&b)) ||
            !fields[0].as_bytes()[0].is_ascii_digit() ||
            fields[2..].iter().any(|value| !value.is_empty() &&
                (!value.as_bytes()[0].is_ascii_digit() || !value.parse::<f64>().is_ok_and(|n| n.is_finite() && n >= 0.0)))
        {
            return Err("性能记录含无效数据行".into());
        }
    }
    if count == 0 { return Err("没有可导出的采样".into()); }
    Ok(())
}

#[tauri::command]
pub async fn export_telemetry_csv(window: tauri::WebviewWindow, csv: String) -> Result<Option<String>, String> {
    if window.label() != "main" { return Err("请从主窗口导出".into()); }
    validate_csv(&csv)?;
    #[cfg(windows)]
    {
        let owner = window.hwnd().map_err(|e| e.to_string())?.0 as usize;
        tauri::async_runtime::spawn_blocking(move || save_csv_dialog(owner, &csv))
            .await.map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    { Err("当前平台未实现性能记录保存对话框".into()) }
}

#[cfg(windows)]
fn save_csv_dialog(owner: usize, csv: &str) -> Result<Option<String>, String> {
    use std::os::windows::ffi::OsStringExt;
    use windows::{
        core::{w, HRESULT},
        Win32::{
            Foundation::{ERROR_CANCELLED, HWND},
            System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED},
            UI::Shell::{Common::COMDLG_FILTERSPEC, FileSaveDialog, IFileSaveDialog, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_OVERWRITEPROMPT, SIGDN_FILESYSPATH},
        },
    };
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) { unsafe { CoUninitialize() } }
    }
    let selected = unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().map_err(|e| e.to_string())?;
        let _apartment = Apartment;
        let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let options = dialog.GetOptions().map_err(|e| e.to_string())?;
        dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR | FOS_OVERWRITEPROMPT).map_err(|e| e.to_string())?;
        dialog.SetTitle(w!("保存 OpenRevo 性能记录")).map_err(|e| e.to_string())?;
        dialog.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: w!("CSV 文件 (*.csv)"), pszSpec: w!("*.csv") }]).map_err(|e| e.to_string())?;
        dialog.SetDefaultExtension(w!("csv")).map_err(|e| e.to_string())?;
        dialog.SetFileName(w!("openrevo-telemetry.csv")).map_err(|e| e.to_string())?;
        if let Err(e) = dialog.Show(HWND(owner as _)) {
            return if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) { Ok(None) } else { Err(e.to_string()) };
        }
        let item = dialog.GetResult().map_err(|e| e.to_string())?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|e| e.to_string())?;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(name.as_wide()));
        CoTaskMemFree(Some(name.0.cast()));
        path
    };
    if !selected.is_absolute() || !selected.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("csv")) {
        return Err("请选择 .csv 文件名".into());
    }
    crate::system::config::persist_file(&selected, csv.as_bytes())
        .map_err(|e| format!("无法保存 {}：{e}", selected.display()))?;
    Ok(Some(selected.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trace_export_accepts_numeric_and_empty_cells_but_not_formulas() {
        let row = "2026-11-19T06:01:00.000Z,0,0,2,42.5,,,,4000,";
        assert!(validate_csv(&format!("{TRACE_HEADER}\r\n{row}\r\n")).is_ok());
        assert!(validate_csv(&format!("{TRACE_HEADER}\n")).is_err());
        assert!(validate_csv(&format!("{TRACE_HEADER}\n{}", row.replace("42.5", "=1+2"))).is_err());
        assert!(validate_csv(&format!("{TRACE_HEADER}\n{}", format!("{row}\n").repeat(601))).is_err());
    }
}
