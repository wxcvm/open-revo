// OpenRevo - Vendor MQTT Control Bus (AiStone)
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

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

fn open() -> Result<TcpStream, String> {
    let addr = broker_addr()?;
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|_| "vendor bus offline".to_string())?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
    stream.set_nodelay(true).ok();
    stream
        .write_all(&connect_packet())
        .map_err(|e| format!("connect write failed: {e}"))?;
    match next_packet(&mut stream) {
        Some((0x20, body)) if body.len() >= 2 && body[1] == 0 => Ok(stream),
        Some((0x20, body)) => Err(format!("broker refused connection: {:?}", body.get(1))),
        _ => Err("no CONNACK from vendor bus".to_string()),
    }
}

fn drain(stream: &mut TcpStream, budget: Duration) -> Vec<BusMessage> {
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

pub fn command(topic: &str, payload: &str, budget: Duration) -> Result<Vec<BusMessage>, String> {
    let started = Instant::now();
    let mut stream = open()?;
    stream.write_all(&subscribe_packet()).ok();
    wait_suback(&mut stream, SUBACK_WAIT.min(budget));
    stream.write_all(&publish_packet(topic, payload)).ok();
    let remaining = budget.saturating_sub(started.elapsed());
    Ok(drain(&mut stream, remaining))
}

pub fn batch(requests: &[(&str, &str)], budget: Duration) -> Result<Vec<BusMessage>, String> {
    let started = Instant::now();
    let mut stream = open()?;
    stream.write_all(&subscribe_packet()).ok();
    wait_suback(&mut stream, SUBACK_WAIT.min(budget));
    for (topic, payload) in requests {
        stream.write_all(&publish_packet(topic, payload)).ok();
    }
    let remaining = budget.saturating_sub(started.elapsed());
    Ok(drain(&mut stream, remaining))
}

pub fn is_available() -> bool {
    open().is_ok()
}

pub fn probe() -> Result<Vec<BusMessage>, String> {
    batch(
        &[
            ("Fan/Control", "{\"Action\":\"GETSTATUS\"}"),
            ("Fan/Control", "{\"Action\":\"GET_FAN_SPEED_CURVE_SETTING\"}"),
        ],
        PROBE_BUDGET,
    )
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
    batch(
        &[
            ("Fan/Control", "{\"Action\":\"OPERATING_CUSTOM_MODE\",\"ProfileIndex\":\"0\"}"),
            ("LCHWOC/Control", "{\"IsCustomRun\":true}"),
        ],
        Duration::from_millis(400),
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
