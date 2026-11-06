// OpenRevo - Vendor MQTT Control Bus (AiStone)
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

mod mqtt;
pub use mqtt::BusMessage;
use mqtt::{
    as_u64, fan_boost_status, BusClient, BROKER_HOST, BROKER_PORT, FAN_STATUS, FAN_TABLE,
    GET_STATUS, SETTING_STATUS,
};

const SESSION_IDLE: Duration = Duration::from_secs(25);
const START_BUDGET: Duration = Duration::from_secs(15);

pub static CONTROL_MODE_LOCK: Mutex<()> = Mutex::new(());
static CLIENT: Mutex<Option<BusClient>> = Mutex::new(None);
static FAN_OBSERVED: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

pub fn observed_fan_boost() -> Option<bool> {
    FAN_OBSERVED
        .lock()
        .as_ref()
        .and_then(|(at, value)| (at.elapsed() < Duration::from_secs(30)).then_some(*value))
}

fn cache_fan_result(result: Result<Vec<BusMessage>, String>) -> Result<Vec<BusMessage>, String> {
    *FAN_OBSERVED.lock() = result.as_ref().ok().and_then(|messages| {
        messages.iter().rev().find_map(|msg| {
            if msg.topic != FAN_STATUS {
                return None;
            }
            let json = serde_json::from_str(&msg.payload).ok()?;
            Some((Instant::now(), fan_boost_status(&json)?))
        })
    });
    result
}

pub(super) fn disconnect() {
    *CLIENT.lock() = None;
    *FAN_OBSERVED.lock() = None;
}

fn broker_addr() -> Result<SocketAddr, String> {
    (BROKER_HOST, BROKER_PORT)
        .to_socket_addrs()
        .map_err(|e| format!("resolve failed: {e}"))?
        .next()
        .ok_or_else(|| "no socket address".to_string())
}

/// Reuse one session; the vendor broker is sensitive to parallel clients and
/// batched commands. Only a transport/framing failure invalidates the session.
fn with_client<T>(work: impl FnOnce(&mut BusClient) -> Result<T, String>) -> Result<T, String> {
    let mut guard = CLIENT.lock();
    if guard
        .as_ref()
        .is_none_or(|client| client.last_used.elapsed() >= SESSION_IDLE)
    {
        *guard = None;
        *guard = Some(BusClient::connect(broker_addr()?)?);
    }
    let result = work(guard.as_mut().expect("connected vendor client"));
    // A complete packet timeout leaves framing intact. Keep that session so
    // an explicit readiness wait does not repeatedly reconnect the same identity.
    if guard.as_ref().is_some_and(|client| !client.is_usable()) {
        *guard = None;
    }
    result
}

fn command(control: &str, payload: &str, reply_topic: &str) -> Result<Vec<BusMessage>, String> {
    with_client(|client| {
        client.request(control, payload, reply_topic, serde_json::Value::is_object)
    })
}

fn backlight_status(json: &serde_json::Value) -> Option<bool> {
    match json.get("SingleColorKBBL")?.as_str()? {
        "SINGLE_COLOR_KBBL_STATUS_ON" => Some(true),
        "SINGLE_COLOR_KBBL_STATUS_OFF" => Some(false),
        _ => None,
    }
}

fn is_available() -> bool {
    with_client(|_| Ok(())).is_ok()
}

pub fn probe_fan() -> Result<Vec<BusMessage>, String> {
    cache_fan_result(with_client(|client| {
        client.request("Fan/Control", GET_STATUS, FAN_STATUS, |json| {
            fan_boost_status(json).is_some()
        })
    }))
}

pub fn probe_keyboard() -> Result<Vec<BusMessage>, String> {
    with_client(|client| {
        client.request("Setting/Control", GET_STATUS, SETTING_STATUS, |json| {
            json.get("CloseTimer").and_then(as_u64).is_some()
        })
    })
}

/// Keyboard backlight auto-off timer. Zero disables it (never auto-off).
/// The vendor field is named "Mins" but the service stores seconds (600 = 10 min).
pub fn set_kb_timer(seconds: u32) -> Result<Vec<BusMessage>, String> {
    let payload = if seconds == 0 {
        r#"{"Action":"KEYBOARD_LIGHTBAR_TIMER_OFF"}"#.to_string()
    } else {
        format!(r#"{{"Action":"KEYBOARD_LIGHTBAR_TIMER_ON","Mins":{seconds}}}"#)
    };
    with_client(|client| {
        client.write_verified("Setting/Control", &payload, SETTING_STATUS, |json| {
            json.get("CloseTimer").and_then(as_u64) == Some(u64::from(seconds))
        })
    })
}

/// Single-colour keyboard backlight on/off switch.
pub fn set_kb_backlight(enabled: bool) -> Result<Vec<BusMessage>, String> {
    let action = if enabled {
        "SINGLE_COLOR_KBBL_STATUS_ON"
    } else {
        "SINGLE_COLOR_KBBL_STATUS_OFF"
    };
    let payload = format!(r#"{{"Action":"{action}"}}"#);
    with_client(|client| {
        client.write_verified("Setting/Control", &payload, SETTING_STATUS, |json| {
            backlight_status(json) == Some(enabled)
        })
    })
}
pub fn set_fan_boost(enabled: bool) -> Result<Vec<BusMessage>, String> {
    let action = if enabled {
        "FAN_BOOST_ON"
    } else {
        "FAN_BOOST_OFF"
    };
    let payload = format!("{{\"Action\":\"{action}\"}}");
    cache_fan_result(with_client(|client| {
        client.write_verified("Fan/Control", &payload, FAN_STATUS, |json| {
            fan_boost_status(json) == Some(enabled)
        })
    }))
}

pub fn set_operating_mode(mode: u8) -> Result<Vec<BusMessage>, String> {
    let action = match mode {
        1 => "OPERATING_OFFICE_MODE",
        2 => "OPERATING_GAMING_MODE",
        3 => "OPERATING_TURBO_MODE",
        _ => "OPERATING_CUSTOM_MODE",
    };
    let payload = format!("{{\"Action\":\"{action}\"}}");
    command("Fan/Control", &payload, FAN_STATUS)
}

pub fn set_fan_table(name: &str, target: &str, duty: &[u8; 16]) -> Result<Vec<BusMessage>, String> {
    let mut payload = format!(
        "{{\"Action\":\"SET_FAN_SPEED_CURVE_SETTING\",\"Name\":\"{name}\",\"Type\":\"{target}\""
    );
    for (index, value) in duty.iter().enumerate() {
        payload.push_str(&format!(",\"T{index}\":\"{value}\""));
    }
    payload.push('}');
    command("Fan/Control", &payload, FAN_TABLE)
}

pub const PRESET_OFFICE: [u8; 16] = [
    0, 30, 30, 35, 45, 45, 50, 55, 55, 55, 55, 55, 55, 55, 55, 55,
];
pub const PRESET_GAMING: [u8; 16] = [
    0, 30, 30, 35, 45, 45, 50, 55, 70, 80, 90, 90, 90, 90, 90, 90,
];
pub const PRESET_TURBO: [u8; 16] = [
    0, 30, 30, 35, 45, 45, 50, 55, 70, 80, 100, 100, 100, 100, 100, 100,
];

pub fn apply_fan_preset(preset: &str) -> Result<Vec<BusMessage>, String> {
    let (label, duty) = match preset.to_ascii_lowercase().as_str() {
        "office" => ("OpenRevo Office", PRESET_OFFICE),
        "gaming" => ("OpenRevo Gaming", PRESET_GAMING),
        "turbo" => ("OpenRevo Turbo", PRESET_TURBO),
        _ => return restore_fan_curve(),
    };
    command(
        "Fan/Control",
        r#"{"Action":"OPERATING_CUSTOM_MODE","ProfileIndex":"0"}"#,
        FAN_STATUS,
    )?;
    std::thread::sleep(Duration::from_millis(300));
    with_client(|client| client.send("LCHWOC/Control", r#"{"IsCustomRun":true}"#))?;
    std::thread::sleep(Duration::from_millis(300));
    set_fan_table(label, "CPU", &duty)?;
    let _ = set_fan_table(label, "GPU", &duty);
    probe_fan()
}

pub fn restore_fan_curve() -> Result<Vec<BusMessage>, String> {
    command(
        "Fan/Control",
        "{\"Action\":\"RESTORE_FAN_SPEED_CURVE_SETTING_ALL\"}",
        FAN_TABLE,
    )
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct VendorServiceState {
    pub present: bool,
    pub selected: bool,
    pub running: bool,
    pub auto_start: bool,
}

fn sc(args: &[&str]) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("sc.exe")
        .args(args)
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| format!("Windows service manager: {e}"))
}

pub(super) fn service_exists(name: &str) -> bool {
    sc(&["query", name]).is_ok_and(|output| output.status.success())
}

pub fn service_state() -> VendorServiceState {
    let cfg = crate::system::config::load_config();
    let Ok(info) = sc(&["qc", "GCUBridge"]) else {
        return VendorServiceState::default();
    };
    if !info.status.success() {
        return VendorServiceState::default();
    }
    VendorServiceState {
        present: true,
        running: sc(&["query", "GCUBridge"]).is_ok_and(|out| {
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("RUNNING")
        }),
        selected: cfg.use_vendor_bus && !cfg.takeover_oem,
        auto_start: String::from_utf8_lossy(&info.stdout).contains("AUTO_START"),
    }
}

fn set_service_persistent(enabled: bool) -> Result<(), String> {
    let mode = if enabled { "auto" } else { "demand" };
    let out = sc(&["config", "GCUBridge", "start=", mode])?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "无法修改 GCUBridge 启动方式（Windows 错误 {}）",
            out.status.code().unwrap_or(-1)
        ))
    }
}

fn start_service(name: &str) -> Result<(), String> {
    let out = sc(&["start", name])?;
    match out.status.code() {
        Some(0 | 1056) => Ok(()), // already running
        code => Err(format!(
            "无法启动 {name}（Windows 错误 {}）",
            code.unwrap_or(-1)
        )),
    }
}

fn ensure_service() -> Result<(), String> {
    if is_available() {
        return Ok(());
    }
    let deadline = Instant::now() + START_BUDGET;
    // On the XINGYAO setup the installed OEM driver is demand-start and the
    // bridge has no SCM dependency on it. Start it first, if registered.
    if service_exists("ACPIDriver") {
        start_service("ACPIDriver")?;
    }
    start_service("GCUBridge")?;
    let addr = broker_addr()?;
    while Instant::now() < deadline {
        // Do not churn MQTT sessions while the service is still initializing.
        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() && is_available() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("GCUBridge 未就绪：15 秒内没有可用的厂商总线".into())
}

pub fn connect_service(persistent: Option<bool>) -> Result<VendorServiceState, String> {
    let _guard = CONTROL_MODE_LOCK.lock();
    if !service_exists("GCUBridge") {
        return Err("未安装 GCUBridge 厂商后台服务".into());
    }
    if persistent != Some(false) {
        ensure_service()?;
    }
    if let Some(enabled) = persistent {
        set_service_persistent(enabled)?;
    }
    if persistent != Some(false) {
        crate::system::config::modify_config(|cfg| cfg.select_vendor_control(true));
    }
    Ok(service_state())
}

/// Explicit user action only. Automatic reconnect never terminates a running worker.
pub fn restart_service() -> Result<VendorServiceState, String> {
    let _guard = CONTROL_MODE_LOCK.lock();
    if !service_exists("GCUBridge") {
        return Err("未安装厂商后台服务".into());
    }
    disconnect();
    let stopped = sc(&["stop", "GCUBridge"])?;
    if !matches!(stopped.status.code(), Some(0 | 1062)) {
        return Err("无法停止厂商后台；请稍后重试".into());
    }
    let result = (|| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = sc(&["query", "GCUBridge"])?;
            if String::from_utf8_lossy(&status.stdout).contains("STOPPED") {
                break;
            }
            if Instant::now() >= deadline {
                return Err("厂商后台未在 10 秒内停止".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        crate::system::process::stop_vendor_worker()?;
        ensure_service()?;
        crate::system::config::modify_config(|cfg| cfg.select_vendor_control(true));
        Ok(service_state())
    })();
    if result.is_err() {
        let _ = start_service("GCUBridge");
    }
    result
}

pub fn recover_service() -> Result<(), String> {
    let _guard = CONTROL_MODE_LOCK.lock();
    let state = service_state();
    if !state.selected || !state.auto_start {
        return Err("厂商通道未设置为常开".into());
    }
    static LAST_RECOVERY: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST_RECOVERY.lock();
    if last.is_some_and(|at| at.elapsed() < Duration::from_secs(30)) {
        return Err("等待下次厂商通道恢复尝试".into());
    }
    let status = sc(&["query", "GCUBridge"])?;
    if !status.status.success() || !String::from_utf8_lossy(&status.stdout).contains("STOPPED") {
        return Err("GCUBridge 未停止，不重复启动".into());
    }
    *last = Some(Instant::now());
    ensure_service()
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_takeover_and_vendor_control_are_mutually_exclusive() {
        let mut config = crate::system::config::AppConfig::default();
        config.select_local_takeover(true);
        config.select_vendor_control(true);
        assert!(config.use_vendor_bus && !config.takeover_oem);
        let mut config: crate::system::config::AppConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert!(config.use_vendor_bus && !config.takeover_oem);
        config.select_local_takeover(true);
        assert!(!config.use_vendor_bus && config.takeover_oem);
    }
}
