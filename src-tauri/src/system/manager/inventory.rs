use super::win;
use serde::Serialize;
use serde_json::Value;
use std::{
    io::Read,
    os::windows::process::CommandExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use windows::Win32::System::{
    SecurityCenter::{
        WSC_SECURITY_PROVIDER_ANTIVIRUS, WSC_SECURITY_PROVIDER_FIREWALL,
        WSC_SECURITY_PROVIDER_HEALTH, WSC_SECURITY_PROVIDER_HEALTH_GOOD,
        WSC_SECURITY_PROVIDER_HEALTH_NOTMONITORED, WSC_SECURITY_PROVIDER_HEALTH_POOR,
        WSC_SECURITY_PROVIDER_HEALTH_SNOOZE,
    },
    SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
};

const LIMIT: u64 = 4 * 1024 * 1024;
pub(super) fn collect(kind: &str, hours: u16) -> win::Outcome<Value> {
    win::verify_account()?;
    if !matches!(kind, "apps" | "startup" | "events" | "health") || !matches!(hours, 1 | 24 | 168) {
        return Err("未知维护数据源".into());
    }
    let powershell = win::system_dir()?.join("WindowsPowerShell/v1.0/powershell.exe");
    let mut child = Command::new(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            include_str!("inventory.ps1"),
        ])
        .env("OPENREVO_MANAGER_QUERY", kind)
        .env("OPENREVO_MANAGER_RANGE", hours.to_string())
        .creation_flags(0x0800_0000)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(win::error)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or("缺少 Windows 数据源输出")?
        .take(LIMIT + 1);
    let mut stderr = child
        .stderr
        .take()
        .ok_or("缺少 Windows 数据源错误输出")?
        .take(8_192);
    let a = std::thread::spawn(move || {
        let mut b = Vec::new();
        stdout.read_to_end(&mut b).map(|_| b)
    });
    let b = std::thread::spawn(move || {
        let mut b = Vec::new();
        stderr.read_to_end(&mut b).map(|_| b)
    });
    let deadline = Instant::now() + Duration::from_secs(18);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(win::error)? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Windows 数据源 18 秒内未响应；此项没有读数".into());
        }
        std::thread::sleep(Duration::from_millis(35));
    };
    let bytes = a
        .join()
        .map_err(|_| "数据源读取线程中止")?
        .map_err(win::error)?;
    let error = b
        .join()
        .map_err(|_| "错误读取线程中止")?
        .map_err(win::error)?;
    if !status.success() || bytes.len() as u64 > LIMIT {
        return Err(format!(
            "Windows 数据源 {kind} 未完成（{status}）：{}",
            String::from_utf8_lossy(&error)
                .chars()
                .take(350)
                .collect::<String>()
        ));
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    serde_json::from_slice::<Value>(bytes).map_err(|e| format!("Windows 数据源返回格式错误：{e}"))
}

#[derive(Serialize)]
pub(crate) struct Health {
    time_ms: u64,
    memory: Option<Memory>,
    antivirus: &'static str,
    firewall: &'static str,
    volumes: Vec<win::Volume>,
}
#[derive(Serialize)]
struct Memory {
    visible: u64,
    available: u64,
    load: u32,
}

fn security(provider: u32) -> &'static str {
    // Windows reports S_FALSE and "poor" when Security Center itself is stopped.
    // The projections swallow successful HRESULT codes; retain S_FALSE with the ABI.
    #[link(name = "wscapi")]
    extern "system" {
        fn WscGetSecurityProviderHealth(
            providers: u32,
            result: *mut WSC_SECURITY_PROVIDER_HEALTH,
        ) -> i32;
    }
    let mut health = WSC_SECURITY_PROVIDER_HEALTH::default();
    if unsafe { WscGetSecurityProviderHealth(provider, &mut health) } != 0 {
        return "unavailable";
    }
    match health {
        WSC_SECURITY_PROVIDER_HEALTH_GOOD => "good",
        WSC_SECURITY_PROVIDER_HEALTH_NOTMONITORED => "not_monitored",
        WSC_SECURITY_PROVIDER_HEALTH_POOR => "poor",
        WSC_SECURITY_PROVIDER_HEALTH_SNOOZE => "snoozed",
        _ => "unavailable",
    }
}

pub(super) fn health() -> win::Outcome<Health> {
    win::verify_account()?;
    let mut m = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    let memory = if unsafe { GlobalMemoryStatusEx(&mut m) }.is_ok() {
        Some(Memory {
            visible: m.ullTotalPhys,
            available: m.ullAvailPhys,
            load: m.dwMemoryLoad,
        })
    } else {
        None
    };
    Ok(Health {
        time_ms: super::now_ms(),
        memory,
        antivirus: security(WSC_SECURITY_PROVIDER_ANTIVIRUS.0 as u32),
        firewall: security(WSC_SECURITY_PROVIDER_FIREWALL.0 as u32),
        volumes: win::volumes(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "real Windows providers, no writes"]
    fn manager_live_read_windows_providers() {
        let base = health().unwrap();
        assert!(base.memory.is_some() && !base.volumes.is_empty());
        for provider in ["health", "apps", "startup", "events"] {
            let record = collect(provider, 24).unwrap_or_else(|e| panic!("{provider}: {e}"));
            let list = record
                .get(if provider == "health" {
                    "updates"
                } else {
                    "rows"
                })
                .unwrap()
                .as_array()
                .unwrap();
            let warnings = record.get("warnings").unwrap().as_array().unwrap();
            if provider == "apps" || provider == "startup" {
                assert!(!list.is_empty(), "{provider} returned an empty inventory");
            }
            eprintln!(
                "{provider}: {} row(s), {} warning(s)",
                list.len(),
                warnings.len()
            );
            for w in warnings {
                eprintln!("  {w}");
            }
        }
    }
}
