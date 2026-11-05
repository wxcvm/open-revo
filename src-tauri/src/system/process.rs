use windows::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// Check if official Mechrevo/Uniwill background processes are actively running.
/// We check every companion process (including GCUService, GCUBridge, AiStoneService, etc.).
/// If even ONE of them is still running (e.g., user disabled GCUService with Huorong but GCUBridge is still active),
/// this returns true so the user is given the option to take over and stop it!
use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

pub fn is_oem_process_name(name: &str) -> bool {
    matches!(
        name,
        "gcubridge.exe"
            | "gcuservice.exe"
            | "aistoneservice.exe"
            | "aistone.exe"
            | "mycontrolcenter.exe"
            | "controlcenteru.exe"
            | "gamingcenteru.exe"
            | "controlcenter.exe"
            | "gamingcenter.exe"
            | "gamingcenter3_cross.exe"
            | "systraycomponent.exe"
            | "ccuwinui.exe"
            | "systemhelper.exe"
            | "defaulttool.exe"
            | "uniwillservice.exe"
            | "unwservice.exe"
            | "osdtpdetect.exe"
            | "displayutil.exe"
            | "sysconutil.exe"
            | "bldhotkeyservice.exe"
            | "bldfnhotkeyutility.exe"
            | "蛟龙游戏控制中心.exe"
            | "jiaolongcc.exe"
    )
}

/// Force terminate all official OEM background processes via pure Win32 API.
/// Takes < 2ms without creating any external taskkill.exe subprocesses!
pub fn kill_oem_processes_native() -> usize {
    let mut killed = 0;
    unsafe {
        if let Ok(handle) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            if handle == INVALID_HANDLE_VALUE {
                return 0;
            }

            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

            if Process32FirstW(handle, &mut entry).is_ok() {
                loop {
                    let len = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();

                    if is_oem_process_name(&name) {
                        if let Ok(p_handle) = OpenProcess(PROCESS_TERMINATE, false, entry.th32ProcessID) {
                            if TerminateProcess(p_handle, 1).is_ok() {
                                killed += 1;
                            }
                            let _ = CloseHandle(p_handle);
                        }
                    }

                    if Process32NextW(handle, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(handle);
        }
    }
    killed
}

/// Check if official Mechrevo/Uniwill background processes are actively running.
/// We check every companion process (including GCUService, GCUBridge, AiStoneService, etc.).
/// If even ONE of them is still running (e.g., user disabled GCUService with Huorong but GCUBridge is still active),
/// this returns true so the user is given the option to take over and stop it!
pub fn is_oem_service_running() -> bool {
    unsafe {
        if let Ok(handle) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            if handle == INVALID_HANDLE_VALUE {
                return false;
            }

            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

            if Process32FirstW(handle, &mut entry).is_ok() {
                loop {
                    let len = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();

                    if is_oem_process_name(&name) {
                        let _ = CloseHandle(handle);
                        return true;
                    }

                    if Process32NextW(handle, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(handle);
        }
    }
    false
}

/// Check if official Mechrevo/Uniwill Control Center is installed on this system.
/// Uses 3-tier validation: Windows Service registry keys, physical directory presence, and active processes.
/// If the machine has NEVER installed official software (clean Windows install), this returns false
/// and the "接管/恢复官方控制台" button will be completely hidden from the UI!
pub fn is_oem_installed() -> bool {
    use std::path::Path;
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ,
    };

    // Tier 1: Windows Service Registry Keys
    // Even if disabled by Huorong/antivirus or stopped in services.msc, the registry entry remains intact.
    let service_keys = [
        r"SYSTEM\CurrentControlSet\Services\GCUBridge",
        r"SYSTEM\CurrentControlSet\Services\GCUService",
        r"SYSTEM\CurrentControlSet\Services\AiStoneService",
        r"SYSTEM\CurrentControlSet\Services\UniwillService",
        r"SYSTEM\CurrentControlSet\Services\OEMService",
    ];

    for key in service_keys {
        let hkey = HSTRING::from(key);
        let mut subkey = windows::Win32::System::Registry::HKEY::default();
        let res = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                windows::core::PCWSTR(hkey.as_ptr()),
                0,
                KEY_READ,
                &mut subkey,
            )
        };
        if res.is_ok() {
            let _ = unsafe { RegCloseKey(subkey) };
            return true;
        }
    }

    // Tier 2: Physical OEM Installation Directories
    let known_paths = [
        r"C:\Program Files\OEM\智控中心",
        r"C:\Program Files\OEM\机械革命电竞控制台",
        r"C:\Program Files\OEM\机械革命控制中心",
        r"C:\Program Files\OEM\AiStoneService",
        r"C:\Program Files\OEM\ControlCenter",
        r"C:\Program Files\OEM\GamingCenter",
        r"C:\Program Files (x86)\OEM",
        r"C:\Program Files\Uniwill",
        r"C:\Program Files (x86)\Uniwill",
        r"C:\Program Files\ControlCenter",
        r"C:\Program Files (x86)\ControlCenter",
    ];

    for path in known_paths {
        if Path::new(path).exists() {
            return true;
        }
    }

    // Check if C:\Program Files\OEM exists and contains any non-empty subdirectories
    if let Ok(entries) = std::fs::read_dir(r"C:\Program Files\OEM") {
        if entries.filter_map(|e| e.ok()).next().is_some() {
            return true;
        }
    }

    // Tier 3: Check UWP AppModel registry (CCU.WinUI on 50 series, ControlCenter3 on 40/50 series)
    let uwp_keys = [
        r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\SystemAppData\CCU.WinUI_wrbgcf7aesyd8",
        r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\SystemAppData\ControlCenter3_wrbgcf7aesyd8",
        r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\SystemAppData\ControlCenter3_h329z55cwnj8g",
    ];
    for key in uwp_keys {
        let hkey = HSTRING::from(key);
        let mut subkey = windows::Win32::System::Registry::HKEY::default();
        let res = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                windows::core::PCWSTR(hkey.as_ptr()),
                0,
                KEY_READ,
                &mut subkey,
            )
        };
        if res.is_ok() {
            let _ = unsafe { RegCloseKey(subkey) };
            return true;
        }
    }

    // Tier 4: If any OEM process is actively running in memory, it is definitely installed
    if is_oem_service_running() {
        return true;
    }

    false
}

/// Check if a specific process is actively running (case-insensitive), returning its PID if found.
pub fn is_process_running(target_name: &str) -> Option<u32> {
    let lower_target = target_name.to_lowercase();
    unsafe {
        if let Ok(handle) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            if handle == INVALID_HANDLE_VALUE {
                return None;
            }

            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

            if Process32FirstW(handle, &mut entry).is_ok() {
                loop {
                    let len = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();

                    if name == lower_target {
                        let pid = entry.th32ProcessID;
                        let _ = CloseHandle(handle);
                        return Some(pid);
                    }

                    if Process32NextW(handle, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(handle);
        }
    }
    None
}

/// Retrieve all currently running OEM processes with their names and PIDs.
pub fn get_running_oem_processes() -> Vec<(String, u32)> {
    let mut procs = Vec::new();
    unsafe {
        if let Ok(handle) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            if handle != INVALID_HANDLE_VALUE {
                let mut entry: PROCESSENTRY32W = std::mem::zeroed();
                entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

                if Process32FirstW(handle, &mut entry).is_ok() {
                    loop {
                        let len = entry
                            .szExeFile
                            .iter()
                            .position(|&c| c == 0)
                            .unwrap_or(entry.szExeFile.len());
                        let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();

                        if is_oem_process_name(&name) {
                            procs.push((name, entry.th32ProcessID));
                        }

                        if Process32NextW(handle, &mut entry).is_err() {
                            break;
                        }
                    }
                }
                let _ = CloseHandle(handle);
            }
        }
    }
    procs
}


/// Stop only the verified AiStone worker after its broker service has stopped.
/// A bridge-only restart can leave this interactive worker holding a dead session.
pub fn stop_vendor_worker() -> Result<(), String> {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{
        QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    let expected = r"C:\Program Files\OEM\机械革命控制中心\AiStoneService\MyControlCenter\GCUService.exe";
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(|e| e.to_string())?;
        let result: Result<(), String> = (|| {
            let mut entry = PROCESSENTRY32W::default();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snapshot, &mut entry).is_err() { return Ok(()); }
            loop {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case("GCUService.exe") {
                    let handle = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, entry.th32ProcessID)
                        .map_err(|e| format!("无法访问厂商控制进程：{e}"))?;
                    let stopped: Result<(), String> = (|| {
                        let mut path = vec![0u16; 32768];
                        let mut len = path.len() as u32;
                        QueryFullProcessImageNameW(handle, PROCESS_NAME_FORMAT(0), PWSTR(path.as_mut_ptr()), &mut len)
                            .map_err(|e| format!("无法核实厂商控制进程路径：{e}"))?;
                        if !String::from_utf16_lossy(&path[..len as usize]).eq_ignore_ascii_case(expected) {
                            return Err("厂商控制进程路径与已知安装位置不同，未终止进程".into());
                        }
                        TerminateProcess(handle, 0).map_err(|e| format!("无法停止厂商控制进程：{e}"))?;
                        if WaitForSingleObject(handle, 5000).0 != 0 {
                            return Err("厂商控制进程未在 5 秒内退出".into());
                        }
                        Ok(())
                    })();
                    let _ = CloseHandle(handle);
                    stopped?;
                }
                if Process32NextW(snapshot, &mut entry).is_err() { break; }
            }
            Ok(())
        })();
        let _ = CloseHandle(snapshot);
        result
    }
}
