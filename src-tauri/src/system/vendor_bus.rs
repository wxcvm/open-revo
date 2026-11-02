// OpenRevo - Vendor MQTT Control Bus (AiStone)
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

const BROKER_HOST: &str = "127.0.0.1";
const BROKER_PORT: u16 = 13688;
const CLIENT_USER: &str = "UWPClient_User_5";
const CLIENT_PASS: &str = "UWPClient_Pwd888881772688_5";
const CLIENT_ID: &str = "UWPClient_5";

const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const READ_TIMEOUT: Duration = Duration::from_millis(200);
pub const REQUEST_BUDGET: Duration = Duration::from_millis(600);
pub const PROBE_BUDGET: Duration = Duration::from_millis(700);
const SUBACK_WAIT: Duration = Duration::from_millis(80);
/// Once a reply arrives, stop after this much silence instead of burning the whole budget.
const IDLE_AFTER_REPLY: Duration = Duration::from_millis(150);

const SUBSCRIBE_TOPICS: &[&str] = &["Fan/Status", "Fan/Table", "Keyboard/Status", "Setting/Status"];

fn encode_len(mut len: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
}

fn push_str(buffer: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    buffer.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    buffer.extend_from_slice(bytes);
}

fn packet(fixed_header: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(fixed_header);
    encode_len(body.len(), &mut out);
    out.extend_from_slice(body);
    out
}

fn connect_packet() -> Vec<u8> {
    let mut body = Vec::new();
    push_str(&mut body, "MQTT");
    body.push(0x04);
    body.push(0xC2);
    body.extend_from_slice(&30u16.to_be_bytes());
    push_str(&mut body, CLIENT_ID);
    push_str(&mut body, CLIENT_USER);
    push_str(&mut body, CLIENT_PASS);
    packet(0x10, &body)
}

fn subscribe_packet() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&1u16.to_be_bytes());
    for topic in SUBSCRIBE_TOPICS {
        push_str(&mut body, topic);
        body.push(1);
    }
    packet(0x82, &body)
}

fn publish_packet(topic: &str, payload: &str) -> Vec<u8> {
    let mut body = Vec::new();
    push_str(&mut body, topic);
    body.extend_from_slice(payload.as_bytes());
    packet(0x30, &body)
}

pub struct BusMessage {
    pub topic: String,
    pub payload: String,
}

fn broker_addr() -> Result<SocketAddr, String> {
    (BROKER_HOST, BROKER_PORT)
        .to_socket_addrs()
        .map_err(|e| format!("resolve failed: {e}"))?
        .next()
        .ok_or_else(|| "no socket address".to_string())
}

fn read_remaining(stream: &mut TcpStream) -> Option<usize> {
    let mut multiplier = 1usize;
    let mut value = 0usize;
    for _ in 0..4 {
        let mut byte = [0u8; 1];
        if stream.read_exact(&mut byte).is_err() {
            return None;
        }
        value += (byte[0] & 0x7F) as usize * multiplier;
        if byte[0] & 0x80 == 0 {
            return Some(value);
        }
        multiplier *= 128;
    }
    None
}

fn next_packet(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut header = [0u8; 1];
    if stream.read_exact(&mut header).is_err() {
        return None;
    }
    let len = read_remaining(stream)?;
    let mut body = vec![0u8; len];
    if len > 0 && stream.read_exact(&mut body).is_err() {
        return None;
    }
    Some((header[0], body))
}

fn parse_publish(body: &[u8]) -> Option<BusMessage> {
    if body.len() < 2 {
        return None;
    }
    let topic_len = u16::from_be_bytes([body[0], body[1]]) as usize;
    if body.len() < 2 + topic_len {
        return None;
    }
    Some(BusMessage {
        topic: String::from_utf8_lossy(&body[2..2 + topic_len]).to_string(),
        payload: String::from_utf8_lossy(&body[2 + topic_len..]).to_string(),
    })
}

fn wait_suback(stream: &mut TcpStream, budget: Duration) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        match next_packet(stream) {
            Some((0x90, _)) => return,
            Some(_) => {}
            None => return,
        }
    }
}

fn drain(stream: &mut TcpStream, budget: Duration, min_replies: usize) -> Vec<BusMessage> {
    let deadline = Instant::now() + budget;
    let mut messages: Vec<BusMessage> = Vec::new();
    let mut last_message_at = Instant::now();
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(now).min(READ_TIMEOUT);
        if stream.set_read_timeout(Some(remaining)).is_err() {
            break;
        }
        match next_packet(stream) {
            Some((header, body)) => {
                if header & 0xF0 == 0x30 {
                    if let Some(message) = parse_publish(&body) {
                        messages.push(message);
                        last_message_at = Instant::now();
                        if messages.len() >= min_replies {
                            break;
                        }
                    }
                }
            }
            None => {
                if !messages.is_empty()
                    && Instant::now().saturating_duration_since(last_message_at) >= IDLE_AFTER_REPLY
                {
                    break;
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
        }
    }
    messages
}
struct BusClient {
    stream: TcpStream,
}

fn connect_client() -> Result<BusClient, String> {
    let addr = broker_addr()?;
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|_| "vendor bus offline".to_string())?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
    stream.set_nodelay(true).ok();
    stream
        .write_all(&connect_packet())
        .map_err(|e| format!("connect write failed: {e}"))?;
    match next_packet(&mut stream) {
        Some((0x20, body)) if body.len() >= 2 && body[1] == 0 => {}
        Some((0x20, body)) => return Err(format!("broker refused connection: {:?}", body.get(1))),
        _ => return Err("no CONNACK from vendor bus".to_string()),
    }
    stream.write_all(&subscribe_packet()).ok();
    wait_suback(&mut stream, SUBACK_WAIT);
    Ok(BusClient { stream })
}

/// 单连接长驻：厂商服务对“频繁建连/断连”很敏感（会直接 CLR 崩溃），
/// 所以这里复用一条连接，出错才重连。
fn with_client<T>(work: impl FnOnce(&mut BusClient) -> Result<T, String>) -> Result<T, String> {
    static CLIENT: OnceLock<Mutex<Option<BusClient>>> = OnceLock::new();
    let mut guard = CLIENT.get_or_init(|| Mutex::new(None)).lock();
    if guard.is_none() {
        *guard = Some(connect_client()?);
    }
    let client = match guard.as_mut() {
        Some(client) => client,
        None => return Err("vendor bus offline".to_string()),
    };
    match work(client) {
        Ok(value) => Ok(value),
        Err(error) => {
            *guard = None;
            Err(error)
        }
    }
}

fn publish_then_drain(client: &mut BusClient, request: (&str, &str), budget: Duration, min_replies: usize) -> Result<Vec<BusMessage>, String> {
    client
        .stream
        .write_all(&publish_packet(request.0, request.1))
        .map_err(|e| format!("publish failed: {e}"))?;
    Ok(drain(&mut client.stream, budget, min_replies))
}

pub fn command(topic: &str, payload: &str, budget: Duration) -> Result<Vec<BusMessage>, String> {
    with_client(|client| publish_then_drain(client, (topic, payload), budget, 1))
}

#[allow(dead_code)]
pub fn batch(requests: &[(&str, &str)], budget: Duration) -> Result<Vec<BusMessage>, String> {
    let started = Instant::now();
    with_client(|client| {
        for request in requests {
            client
                .stream
                .write_all(&publish_packet(request.0, request.1))
                .map_err(|e| format!("publish failed: {e}"))?;
        }
        let remaining = budget.saturating_sub(started.elapsed());
        Ok(drain(&mut client.stream, remaining, requests.len()))
    })
}

pub fn is_available() -> bool {
    connect_client().is_ok()
}
pub fn probe() -> Result<Vec<BusMessage>, String> {
    // 逐条发送：厂商 MQTT 服务在一条连接上连收多条命令时会抛
    // NullReferenceException（MqttServer_ApplicationMessageReceived），把 broker 带崩。
    let mut result = command(
        "Fan/Control",
        "{\"Action\":\"GETSTATUS\"}",
        Duration::from_millis(500),
    )?;
    if let Ok(mut extra) = command(
        "Fan/Control",
        "{\"Action\":\"GET_FAN_SPEED_CURVE_SETTING\"}",
        Duration::from_millis(450),
    ) {
        result.append(&mut extra);
    }
    if let Ok(mut extra) = command(
        "Setting/Control",
        "{\"Action\":\"GETSTATUS\"}",
        Duration::from_millis(450),
    ) {
        result.append(&mut extra);
    }
    Ok(result)
}
/// Keyboard backlight auto-off timer. Zero disables it (never auto-off).
/// The vendor field is named "Mins" but the service stores seconds (600 = 10 min).
pub fn set_kb_timer(seconds: u32) -> Result<Vec<BusMessage>, String> {
    let payload = if seconds == 0 {
        r#"{"Action":"KEYBOARD_LIGHTBAR_TIMER_OFF"}"#.to_string()
    } else {
        format!(r#"{{"Action":"KEYBOARD_LIGHTBAR_TIMER_ON","Mins":{seconds}}}"#)
    };
    command("Setting/Control", &payload, REQUEST_BUDGET)
}

/// Single-colour keyboard backlight on/off switch.
pub fn set_kb_backlight(enabled: bool) -> Result<Vec<BusMessage>, String> {
    let action = if enabled { "SINGLE_COLOR_KBBL_STATUS_ON" } else { "SINGLE_COLOR_KBBL_STATUS_OFF" };
    let payload = format!(r#"{{"Action":"{action}"}}"#);
    command("Setting/Control", &payload, REQUEST_BUDGET)
}
pub fn set_fan_boost(enabled: bool) -> Result<Vec<BusMessage>, String> {
    let action = if enabled { "FAN_BOOST_ON" } else { "FAN_BOOST_OFF" };
    let payload = format!("{{\"Action\":\"{action}\"}}");
    command("Fan/Control", &payload, REQUEST_BUDGET)
}

pub fn set_operating_mode(mode: u8) -> Result<Vec<BusMessage>, String> {
    let action = match mode {
        1 => "OPERATING_OFFICE_MODE",
        2 => "OPERATING_GAMING_MODE",
        3 => "OPERATING_TURBO_MODE",
        _ => "OPERATING_CUSTOM_MODE",
    };
    let payload = format!("{{\"Action\":\"{action}\"}}");
    command("Fan/Control", &payload, REQUEST_BUDGET)
}

pub fn set_fan_table(name: &str, target: &str, duty: &[u8; 16]) -> Result<Vec<BusMessage>, String> {
    let mut payload = format!(
        "{{\"Action\":\"SET_FAN_SPEED_CURVE_SETTING\",\"Name\":\"{name}\",\"Type\":\"{target}\""
    );
    for (index, value) in duty.iter().enumerate() {
        payload.push_str(&format!(",\"T{index}\":\"{value}\""));
    }
    payload.push('}');
    command("Fan/Control", &payload, REQUEST_BUDGET)
}

pub const PRESET_OFFICE: [u8; 16] = [0, 30, 30, 35, 45, 45, 50, 55, 55, 55, 55, 55, 55, 55, 55, 55];
pub const PRESET_GAMING: [u8; 16] = [0, 30, 30, 35, 45, 45, 50, 55, 70, 80, 90, 90, 90, 90, 90, 90];
pub const PRESET_TURBO: [u8; 16] = [0, 30, 30, 35, 45, 45, 50, 55, 70, 80, 100, 100, 100, 100, 100, 100];

pub fn apply_fan_preset(preset: &str) -> Result<Vec<BusMessage>, String> {
    let (label, duty) = match preset.to_ascii_lowercase().as_str() {
        "office" => ("OpenRevo Office", PRESET_OFFICE),
        "gaming" => ("OpenRevo Gaming", PRESET_GAMING),
        "turbo" => ("OpenRevo Turbo", PRESET_TURBO),
        _ => return restore_fan_curve(),
    };
    // 逐条发送：连发会触发厂商 broker 的 NullReferenceException 崩溃
    command(
        "Fan/Control",
        "{\"Action\":\"OPERATING_CUSTOM_MODE\",\"ProfileIndex\":\"0\"}",
        Duration::from_millis(400),
    )?;
    std::thread::sleep(Duration::from_millis(300));
    command(
        "LCHWOC/Control",
        "{\"IsCustomRun\":true}",
        Duration::from_millis(300),
    )?;
    set_fan_table(label, "CPU", &duty)?;
    let _ = set_fan_table(label, "GPU", &duty);
    probe()
}

pub fn restore_fan_curve() -> Result<Vec<BusMessage>, String> {
    command(
        "Fan/Control",
        "{\"Action\":\"RESTORE_FAN_SPEED_CURVE_SETTING_ALL\"}",
        REQUEST_BUDGET,
    )
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct VendorServiceState {
    pub present: bool,
    pub running: bool,
    pub auto_start: bool,
}

fn sc_query_contains(args: &[&str], needle: &str) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        if let Ok(output) = std::process::Command::new("sc.exe")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            return text.contains(needle);
        }
        false
    }
    #[cfg(not(windows))]
    {
        let _ = (args, needle);
        false
    }
}

/// Service presence / runtime state, used to decide who owns the vendor stack.
pub fn service_state() -> VendorServiceState {
    let present = vendor_stack_present();
    if !present {
        return VendorServiceState::default();
    }
    VendorServiceState {
        present,
        running: sc_query_contains(&["query", "GCUBridge"], "RUNNING"),
        auto_start: sc_query_contains(&["qc", "GCUBridge"], "AUTO_START"),
    }
}

/// Whether the vendor control-center binaries are still installed.
/// After an uninstall the service and broker are gone, so no bus can ever come up.
pub fn vendor_stack_present() -> bool {
    std::path::Path::new(
        r"C:\Program Files\OEM\机械革命控制中心\AiStoneService\MyControlCenter\GCUService.exe",
    )
    .exists()
}

/// Switch the vendor service between Automatic and Manual so the bus stops flapping.
pub fn set_service_persistent(enabled: bool) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mode = if enabled { "auto" } else { "demand" };
        let status = std::process::Command::new("sc.exe")
            .args(["config", "GCUBridge", "start=", mode])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        matches!(status, Ok(code) if code.success())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn ensure_service() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("sc.exe")
            .args(["start", "GCUBridge"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(150));
            if is_available() {
                return true;
            }
        }
        false
    }
    #[cfg(not(windows))]
    {
        false
    }
}
