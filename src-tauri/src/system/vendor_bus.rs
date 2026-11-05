// OpenRevo - Vendor MQTT Control Bus (AiStone)
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

const BROKER_HOST: &str = "127.0.0.1";
const BROKER_PORT: u16 = 13688;
const CLIENT_USER: &str = "UWPClient_User_5";
const CLIENT_PASS: &str = "UWPClient_Pwd888881772688_5";
const CLIENT_ID: &str = "UWPClient_5";

const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const REQUEST_BUDGET: Duration = Duration::from_millis(600);
const HANDSHAKE_BUDGET: Duration = Duration::from_millis(400);
const WRITE_TIMEOUT: Duration = Duration::from_millis(200);
const MAX_PACKET: usize = 64 * 1024;
// Reconnect after a quiet page switch rather than reusing an expired MQTT session.
const SESSION_IDLE: Duration = Duration::from_secs(25);
const START_BUDGET: Duration = Duration::from_secs(15);

const FAN_STATUS: &str = "Fan/Status";
const SETTING_STATUS: &str = "Setting/Status";
const FAN_TABLE: &str = "Fan/Table";
const SUBSCRIBE_TOPICS: &[&str] = &[FAN_STATUS, FAN_TABLE, SETTING_STATUS];
const GET_STATUS: &str = r#"{"Action":"GETSTATUS"}"#;

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

fn missing_status_error(topic: &str) -> String {
    let feature = if topic == FAN_STATUS {
        "风扇"
    } else if topic == SETTING_STATUS {
        "键盘"
    } else {
        "设备"
    };
    format!("厂商后台已连接，但未返回可确认的{feature}状态；尚不能确认操作是否生效")
}

pub(super) fn disconnect() {
    *CLIENT.lock() = None;
    *FAN_OBSERVED.lock() = None;
}

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
        body.push(0);
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

fn read_before(stream: &mut TcpStream, mut buf: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !buf.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(buf) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => buf = &mut buf[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn next_packet(stream: &mut TcpStream, deadline: Instant) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0];
    read_before(stream, &mut header, deadline)?;
    // Timeout in the middle of a frame invalidates this socket; it cannot be reused.
    let mut read_body = |buf: &mut [u8]| {
        read_before(stream, buf, deadline)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    };
    let mut len = 0;
    for shift in [0, 7, 14, 21] {
        let mut byte = [0];
        read_body(&mut byte)?;
        len |= usize::from(byte[0] & 0x7f) << shift;
        if len > MAX_PACKET {
            break;
        }
        if byte[0] & 0x80 == 0 {
            let mut body = vec![0; len];
            read_body(&mut body)?;
            return Ok((header[0], body));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid vendor MQTT packet length",
    ))
}

fn parse_publish(body: &[u8]) -> Option<BusMessage> {
    let topic_len = u16::from_be_bytes(body.get(..2)?.try_into().ok()?) as usize;
    Some(BusMessage {
        topic: std::str::from_utf8(body.get(2..2 + topic_len)?)
            .ok()?
            .to_string(),
        payload: std::str::from_utf8(body.get(2 + topic_len..)?)
            .ok()?
            .to_string(),
    })
}

struct BusClient {
    stream: TcpStream,
    last_used: Instant,
}

impl BusClient {
    fn connect(addr: SocketAddr) -> Result<Self, String> {
        let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
            .map_err(|e| format!("vendor bus offline: {e}"))?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(WRITE_TIMEOUT))
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + HANDSHAKE_BUDGET;
        stream
            .write_all(&connect_packet())
            .map_err(|e| e.to_string())?;
        match next_packet(&mut stream, deadline) {
            Ok((0x20, body)) if body == [0, 0] => {}
            _ => return Err("vendor bus did not accept the MQTT connection".into()),
        }
        stream
            .write_all(&subscribe_packet())
            .map_err(|e| e.to_string())?;
        loop {
            match next_packet(&mut stream, deadline) {
                Ok((0x90, body)) if body.starts_with(&[0, 1]) => {
                    if body.len() == 2 + SUBSCRIBE_TOPICS.len()
                        && body[2..].iter().all(|&qos| qos == 0)
                    {
                        return Ok(Self {
                            stream,
                            last_used: Instant::now(),
                        });
                    }
                    return Err("vendor bus refused a status subscription".into());
                }
                Ok(_) => {}
                Err(e) => return Err(format!("vendor MQTT subscription failed: {e}")),
            }
        }
    }

    fn send(&mut self, topic: &str, payload: &str) -> Result<(), String> {
        self.stream
            .write_all(&publish_packet(topic, payload))
            .map_err(|e| format!("vendor publish failed: {e}"))?;
        self.last_used = Instant::now();
        Ok(())
    }

    fn receive(
        &mut self,
        topic: &str,
        accept: impl Fn(&serde_json::Value) -> bool,
    ) -> Result<Option<BusMessage>, String> {
        let deadline = Instant::now() + REQUEST_BUDGET;
        loop {
            match next_packet(&mut self.stream, deadline) {
                // Subscriptions request QoS 0. Ignore retained values; they are not command replies.
                Ok((header, body)) if header == 0x30 => {
                    if let Some(msg) = parse_publish(&body) {
                        if msg.topic == topic
                            && serde_json::from_str(&msg.payload).is_ok_and(|json| accept(&json))
                        {
                            return Ok(Some(msg));
                        }
                    }
                }
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Ok(None)
                }
                Err(e) => return Err(format!("vendor MQTT read failed: {e}")),
            }
        }
    }

    fn request(
        &mut self,
        control: &str,
        payload: &str,
        topic: &str,
        accept: impl Fn(&serde_json::Value) -> bool,
    ) -> Result<Vec<BusMessage>, String> {
        self.send(control, payload)?;
        self.receive(topic, accept)?
            .map(|msg| vec![msg])
            .ok_or_else(|| missing_status_error(topic))
    }

    fn write_verified(
        &mut self,
        control: &str,
        payload: &str,
        topic: &str,
        accept: impl Fn(&serde_json::Value) -> bool,
    ) -> Result<Vec<BusMessage>, String> {
        self.send(control, payload)?;
        if let Some(msg) = self.receive(topic, &accept)? {
            return Ok(vec![msg]);
        }
        // Some service builds do not push a status for a write. Read it back once;
        // never retry a write whose outcome is unknown.
        self.request(control, GET_STATUS, topic, accept)
    }
}

/// Reuse one session; the vendor broker is sensitive to parallel clients and
/// batched commands. A failed transaction invalidates the session.
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
    if result.is_err() {
        *guard = None;
    }
    result
}

fn command(control: &str, payload: &str, reply_topic: &str) -> Result<Vec<BusMessage>, String> {
    with_client(|client| {
        client.request(control, payload, reply_topic, serde_json::Value::is_object)
    })
}

fn as_u64(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn fan_boost_status(json: &serde_json::Value) -> Option<bool> {
    let value = json.get("FanBoostEnable")?;
    value.as_bool().or_else(|| match as_u64(value) {
        Some(0) => Some(false),
        Some(1) => Some(true),
        _ => None,
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
    use super::*;
    use std::net::TcpListener;

    fn fixture(
        run: impl FnOnce(TcpStream) + Send + 'static,
    ) -> (BusClient, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind((BROKER_HOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            assert_eq!(read(&mut stream).0, 0x10);
            stream.write_all(&packet(0x20, &[0, 0])).unwrap();
            assert_eq!(read(&mut stream).0, 0x82);
            stream.write_all(&packet(0x90, &[0, 1, 0, 0, 0])).unwrap();
            run(stream);
        });
        (BusClient::connect(addr).unwrap(), worker)
    }

    fn read(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        next_packet(stream, Instant::now() + Duration::from_secs(3)).unwrap()
    }

    fn publish(stream: &mut TcpStream, topic: &str, payload: &str, retained: bool) {
        let mut frame = publish_packet(topic, payload);
        frame[0] |= u8::from(retained);
        stream.write_all(&frame).unwrap();
    }

    #[test]
    fn waits_for_the_expected_status_not_any_publish() {
        let (mut client, worker) = fixture(|mut stream| {
            assert_eq!(read(&mut stream).0, 0x30);
            publish(&mut stream, FAN_STATUS, r#"{"FanBoostEnable":"1"}"#, false);
            publish(&mut stream, SETTING_STATUS, r#"{"CloseTimer":0}"#, true);
            publish(&mut stream, SETTING_STATUS, r#"{"Unrelated":true}"#, false);
            std::thread::sleep(Duration::from_millis(200));
            publish(
                &mut stream,
                SETTING_STATUS,
                r#"{"CloseTimer":"600"}"#,
                false,
            );
        });
        let result = client
            .request("Setting/Control", GET_STATUS, SETTING_STATUS, |json| {
                json.get("CloseTimer").and_then(as_u64).is_some()
            })
            .unwrap();
        assert_eq!(result[0].payload, r#"{"CloseTimer":"600"}"#);
        worker.join().unwrap();
    }

    #[test]
    fn verifies_write_via_readback_without_replaying_it() {
        let (mut client, worker) = fixture(|mut stream| {
            let (_, body) = read(&mut stream);
            assert_eq!(
                parse_publish(&body).unwrap().payload,
                r#"{"Action":"FAN_BOOST_ON"}"#
            );
            publish(&mut stream, FAN_STATUS, r#"{"FanBoostEnable":"0"}"#, false);
            let (_, body) = read(&mut stream);
            assert_eq!(parse_publish(&body).unwrap().payload, GET_STATUS);
            publish(&mut stream, FAN_STATUS, r#"{"FanBoostEnable":1}"#, false);
        });
        let result = client.write_verified(
            "Fan/Control",
            r#"{"Action":"FAN_BOOST_ON"}"#,
            FAN_STATUS,
            |json| fan_boost_status(json) == Some(true),
        );
        assert!(result.is_ok());
        worker.join().unwrap();
    }

    #[test]
    fn refuses_to_report_an_unconfirmed_write_as_success() {
        let (mut client, worker) = fixture(|mut stream| {
            read(&mut stream);
            let (_, body) = read(&mut stream);
            assert_eq!(parse_publish(&body).unwrap().payload, GET_STATUS);
            publish(
                &mut stream,
                SETTING_STATUS,
                r#"{"CloseTimer":"300"}"#,
                false,
            );
            assert!(next_packet(&mut stream, Instant::now() + Duration::from_secs(2)).is_err());
        });
        let result = client.write_verified(
            "Setting/Control",
            r#"{"Action":"KEYBOARD_LIGHTBAR_TIMER_ON","Mins":600}"#,
            SETTING_STATUS,
            |json| json.get("CloseTimer").and_then(as_u64) == Some(600),
        );
        assert_eq!(
            result.err().as_deref(),
            Some(missing_status_error(SETTING_STATUS).as_str())
        );
        drop(client);
        worker.join().unwrap();
    }

    #[test]
    fn disconnect_does_not_look_like_an_empty_success() {
        let (mut client, worker) = fixture(|mut stream| {
            read(&mut stream);
        });
        let error = client
            .request(
                "Fan/Control",
                GET_STATUS,
                FAN_STATUS,
                serde_json::Value::is_object,
            )
            .err()
            .unwrap();
        assert!(error.contains("MQTT read failed"), "{error}");
        worker.join().unwrap();
    }

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
