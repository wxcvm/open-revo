// OpenRevo - Vendor MQTT Control Bus (AiStone / 机械革命控制中心)
//
// The official control center exposes its hardware bus as a local MQTT broker
// (localhost:13688, credentials below are the UWP client's) and accepts JSON
// commands on topics such as "Fan/Control", "Keyboard/Ctrl", "System/Control".
// Talking to that broker is the only *sanctioned* way to reach fan curves and
// keyboard backlight levels on models whose WMI mailbox has no write channel
// (e.g. XINGYAO / Strix Point, see docs/xingyao-wmi-map.md).
//
// Everything here is opt-in: when the vendor service is not running the probe
// simply reports "offline" and no packet is sent.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

const BROKER_HOST: &str = "127.0.0.1";
const BROKER_PORT: u16 = 13688;
const CLIENT_USER: &str = "UWPClient_User_5";
const CLIENT_PASS: &str = "UWPClient_Pwd888881772688_5";
const CLIENT_ID: &str = "UWPClient_5";

const SUBSCRIBE_TOPICS: &[&str] = &[
    "Fan/Status",
    "Fan/Table",
    "Keyboard/Status",
    "Setting/Status",
    "System/FanInfo",
];

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

struct Reader<'a> {
    stream: &'a mut TcpStream,
}

impl<'a> Reader<'a> {
    fn read_remaining(&mut self) -> Option<usize> {
        let mut multiplier = 1usize;
        let mut value = 0usize;
        for _ in 0..4 {
            let mut byte = [0u8; 1];
            if self.stream.read_exact(&mut byte).is_err() {
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

    fn next_packet(&mut self) -> Option<(u8, Vec<u8>)> {
        let mut header = [0u8; 1];
        if self.stream.read_exact(&mut header).is_err() {
            return None;
        }
        let len = self.read_remaining()?;
        let mut body = vec![0u8; len];
        if len > 0 && self.stream.read_exact(&mut body).is_err() {
            return None;
        }
        Some((header[0], body))
    }
}

fn parse_publish(body: &[u8]) -> Option<BusMessage> {
    if body.len() < 2 {
        return None;
    }
    let topic_len = u16::from_be_bytes([body[0], body[1]]) as usize;
    if body.len() < 2 + topic_len {
        return None;
    }
    let topic = String::from_utf8_lossy(&body[2..2 + topic_len]).to_string();
    let payload = String::from_utf8_lossy(&body[2 + topic_len..]).to_string();
    Some(BusMessage { topic, payload })
}

pub fn command(topic: &str, payload: &str, wait: Duration) -> Result<Vec<BusMessage>, String> {
    let addr = (BROKER_HOST, BROKER_PORT)
        .to_socket_addrs()
        .map_err(|e| format!("resolve failed: {e}"))?
        .next()
        .ok_or_else(|| "no socket address".to_string())?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(1200))
        .map_err(|_| "vendor bus offline".to_string())?;
    stream.set_read_timeout(Some(Duration::from_millis(400))).ok();
    stream.set_nodelay(true).ok();

    stream
        .write_all(&connect_packet())
        .map_err(|e| format!("connect write failed: {e}"))?;
    {
        let mut reader = Reader { stream: &mut stream };
        match reader.next_packet() {
            Some((0x20, body)) if body.len() >= 2 && body[1] == 0 => {}
            Some((0x20, body)) => {
                return Err(format!("broker refused connection: {:?}", body.get(1)))
            }
            _ => return Err("no CONNACK from vendor bus".to_string()),
        }
    }
    stream.write_all(&subscribe_packet()).ok();
    stream.write_all(&publish_packet(topic, payload)).ok();

    let deadline = Instant::now() + wait;
    let mut messages = Vec::new();
    let mut reader = Reader { stream: &mut stream };
    while Instant::now() < deadline {
        if let Some((header, body)) = reader.next_packet() {
            if header & 0xF0 == 0x30 {
                if let Some(message) = parse_publish(&body) {
                    messages.push(message);
                }
            }
        }
    }
    Ok(messages)
}

/// Start the vendor service host (GCUBridge) if it is not running.
/// open-revo is itself elevated, so this only touches the vendor service.
pub fn ensure_service() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("sc.exe")
            .args(["start", "GCUBridge"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(250));
            let target = std::net::SocketAddr::from(([127, 0, 0, 1], BROKER_PORT));
            if TcpStream::connect_timeout(&target, Duration::from_millis(300)).is_ok() {
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


/// Vendor default curves (UserFanTables/DefaultFanTable_*.json -> Duty arrays).
pub const PRESET_OFFICE: [u8; 16] = [0, 30, 30, 35, 45, 45, 50, 55, 55, 55, 55, 55, 55, 55, 55, 55];
pub const PRESET_GAMING: [u8; 16] = [0, 30, 30, 35, 45, 45, 50, 55, 70, 80, 90, 90, 90, 90, 90, 90];
pub const PRESET_TURBO: [u8; 16] = [0, 30, 30, 35, 45, 45, 50, 55, 70, 80, 100, 100, 100, 100, 100, 100];

fn publish(topic: &str, payload: &str, wait_ms: u64) -> Result<Vec<BusMessage>, String> {
    command(topic, payload, Duration::from_millis(wait_ms))
}

/// Switch to the custom operating mode first (the vendor only accepts a written
/// curve while the machine sits in custom mode), then push CPU and GPU curves.
pub fn apply_fan_preset(preset: &str) -> Result<Vec<BusMessage>, String> {
    let (label, duty) = match preset.to_ascii_lowercase().as_str() {
        "office" => ("OpenRevo Office", PRESET_OFFICE),
        "gaming" => ("OpenRevo Gaming", PRESET_GAMING),
        "turbo" => ("OpenRevo Turbo", PRESET_TURBO),
        _ => return restore_fan_curve(),
    };

    let mut messages = publish("Fan/Control", "{\"Action\":\"OPERATING_CUSTOM_MODE\"}", 1200)?;
    messages.extend(set_fan_table(label, "CPU", &duty)?);
    // GPU table is accepted even without a discrete GPU; keep both in sync.
    let _ = set_fan_table(label, "GPU", &duty);
    messages.extend(probe().unwrap_or_default());
    Ok(messages)
}

/// Hand control back to the vendor firmware (drops the custom table).
pub fn restore_fan_curve() -> Result<Vec<BusMessage>, String> {
    let mut messages = publish(
        "Fan/Control",
        "{\"Action\":\"RESTORE_FAN_SPEED_CURVE_SETTING_ALL\"}",
        1200,
    )?;
    messages.extend(probe().unwrap_or_default());
    Ok(messages)
}

pub fn probe() -> Result<Vec<BusMessage>, String> {
    let mut result = command("Fan/Control", "{\"Action\":\"GETSTATUS\"}", Duration::from_millis(900))?;
    if result.is_empty() {
        // 服务刚被拉起时首个请求可能落在预热窗口里，重试一次
        std::thread::sleep(Duration::from_millis(900));
        result = command("Fan/Control", "{\"Action\":\"GETSTATUS\"}", Duration::from_millis(1200))?;
    }
    if let Ok(mut table) = command(
        "Fan/Control",
        "{\"Action\":\"GET_FAN_SPEED_CURVE_SETTING\"}",
        Duration::from_millis(700),
    ) {
        result.append(&mut table);
    }
    Ok(result)
}

pub fn set_fan_boost(enabled: bool) -> Result<Vec<BusMessage>, String> {
    let action = if enabled { "FAN_BOOST_ON" } else { "FAN_BOOST_OFF" };
    let payload = format!("{{\"Action\":\"{action}\"}}");
    command("Fan/Control", &payload, Duration::from_millis(800))
}

pub fn set_operating_mode(mode: u8) -> Result<Vec<BusMessage>, String> {
    let action = match mode {
        1 => "OPERATING_OFFICE_MODE",
        2 => "OPERATING_GAMING_MODE",
        3 => "OPERATING_TURBO_MODE",
        _ => "OPERATING_CUSTOM_MODE",
    };
    let payload = format!("{{\"Action\":\"{action}\"}}");
    command("Fan/Control", &payload, Duration::from_millis(1500))
}

pub fn set_fan_table(name: &str, target: &str, duty: &[u8; 16]) -> Result<Vec<BusMessage>, String> {
    let mut payload = format!(
        "{{\"Action\":\"SET_FAN_SPEED_CURVE_SETTING\",\"Name\":\"{name}\",\"Type\":\"{target}\""
    );
    for (index, value) in duty.iter().enumerate() {
        payload.push_str(&format!(",\"T{index}\":\"{value}\""));
    }
    payload.push('}');
    command("Fan/Control", &payload, Duration::from_millis(1200))
}
