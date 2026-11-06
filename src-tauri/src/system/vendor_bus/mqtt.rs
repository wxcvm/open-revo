// Pure, bounded MQTT transport. Windows service control stays in the facade.
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

pub(crate) const BROKER_HOST: &str = "127.0.0.1";
pub(crate) const BROKER_PORT: u16 = 13688;
const CLIENT_USER: &str = "UWPClient_User_5";
const CLIENT_PASS: &str = "UWPClient_Pwd888881772688_5";
const CLIENT_ID: &str = "UWPClient_5";

const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const REQUEST_BUDGET: Duration = Duration::from_millis(600);
const HANDSHAKE_BUDGET: Duration = Duration::from_millis(400);
const WRITE_TIMEOUT: Duration = Duration::from_millis(200);
const MAX_PACKET: usize = 64 * 1024;

pub(crate) const FAN_STATUS: &str = "Fan/Status";
pub(crate) const SETTING_STATUS: &str = "Setting/Status";
pub(crate) const FAN_TABLE: &str = "Fan/Table";
const SUBSCRIBE_TOPICS: &[&str] = &[FAN_STATUS, FAN_TABLE, SETTING_STATUS];
pub(crate) const GET_STATUS: &str = r#"{"Action":"GETSTATUS"}"#;

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

pub(crate) struct BusClient {
    stream: TcpStream,
    pub(crate) last_used: Instant,
    usable: bool,
}

impl Drop for BusClient {
    fn drop(&mut self) {
        if self.usable {
            // MQTT clean disconnect; a missing device reply is not a dead socket.
            let _ = self.stream.write_all(&packet(0xe0, &[]));
        }
    }
}

impl BusClient {
    pub(crate) fn is_usable(&self) -> bool {
        self.usable
    }

    pub(crate) fn connect(addr: SocketAddr) -> Result<Self, String> {
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
        let mut client = Self {
            stream,
            last_used: Instant::now(),
            usable: true,
        };
        client
            .stream
            .write_all(&subscribe_packet())
            .map_err(|e| e.to_string())?;
        loop {
            match next_packet(&mut client.stream, deadline) {
                Ok((0x90, body)) if body.starts_with(&[0, 1]) => {
                    if body.len() == 2 + SUBSCRIBE_TOPICS.len()
                        && body[2..].iter().all(|&qos| qos == 0)
                    {
                        return Ok(client);
                    }
                    return Err("vendor bus refused a status subscription".into());
                }
                Ok(_) => {}
                Err(e) => {
                    client.usable = false;
                    return Err(format!("vendor MQTT subscription failed: {e}"));
                }
            }
        }
    }

    pub(crate) fn send(&mut self, topic: &str, payload: &str) -> Result<(), String> {
        if let Err(error) = self.stream.write_all(&publish_packet(topic, payload)) {
            self.usable = false;
            return Err(format!("厂商会话写入失败，操作结果未知：{error}"));
        }
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
                Err(e) => {
                    self.usable = false;
                    return Err(format!("vendor MQTT read failed: {e}"));
                }
            }
        }
    }

    pub(crate) fn request(
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

    pub(crate) fn write_verified(
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

pub(crate) fn as_u64(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

pub(crate) fn fan_boost_status(json: &serde_json::Value) -> Option<bool> {
    let value = json.get("FanBoostEnable")?;
    value.as_bool().or_else(|| match as_u64(value) {
        Some(0) => Some(false),
        Some(1) => Some(true),
        _ => None,
    })
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
            assert_eq!(read(&mut stream), (0xe0, vec![]));
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
    fn missing_reply_keeps_a_well_framed_session_reusable() {
        let (mut client, worker) = fixture(|mut stream| {
            let (_, first) = read(&mut stream);
            assert_eq!(parse_publish(&first).unwrap().payload, GET_STATUS);
            // No answer: the next request must use this same accepted socket.
            let (_, second) = read(&mut stream);
            assert_eq!(parse_publish(&second).unwrap().payload, GET_STATUS);
            publish(&mut stream, FAN_STATUS, r#"{"FanBoostEnable":"0"}"#, false);
        });
        assert!(client
            .request("Fan/Control", GET_STATUS, FAN_STATUS, |j| fan_boost_status(
                j
            )
            .is_some())
            .is_err());
        assert!(client.is_usable());
        assert!(client
            .request("Fan/Control", GET_STATUS, FAN_STATUS, |j| fan_boost_status(
                j
            )
            .is_some())
            .is_ok());
        worker.join().unwrap();
    }

    #[test]
    fn healthy_drop_sends_mqtt_disconnect() {
        let (client, worker) = fixture(|mut stream| {
            assert_eq!(read(&mut stream), (0xe0, Vec::new()));
        });
        drop(client);
        worker.join().unwrap();
    }

    #[test]
    fn incomplete_frame_invalidates_the_connection() {
        let (mut client, worker) = fixture(|mut stream| {
            read(&mut stream);
            let frame = publish_packet(FAN_STATUS, r#"{"FanBoostEnable":"0"}"#);
            stream.write_all(&frame[..5]).unwrap();
            std::thread::sleep(Duration::from_millis(950));
        });
        assert!(client
            .request("Fan/Control", GET_STATUS, FAN_STATUS, |_| true)
            .is_err());
        assert!(!client.is_usable());
        worker.join().unwrap();
    }

    #[test]
    fn packet_length_is_bounded_before_allocating_the_body() {
        let (mut client, worker) = fixture(|mut stream| {
            read(&mut stream);
            let mut frame = vec![0x30];
            encode_len(MAX_PACKET + 1, &mut frame);
            stream.write_all(&frame).unwrap();
        });
        let result = client.request("Fan/Control", GET_STATUS, FAN_STATUS, |_| true);
        assert!(result.is_err());
        assert!(!client.is_usable());
        worker.join().unwrap();
    }

    #[test]
    fn fragmented_tcp_delivery_preserves_the_whole_status() {
        let (mut client, worker) = fixture(|mut stream| {
            read(&mut stream);
            let frame = publish_packet(FAN_STATUS, r#"{"FanBoostEnable":"1"}"#);
            for part in frame.chunks(3) {
                stream.write_all(part).unwrap();
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let response = client
            .request("Fan/Control", GET_STATUS, FAN_STATUS, |j| {
                fan_boost_status(j) == Some(true)
            })
            .unwrap();
        assert_eq!(response[0].payload, r#"{"FanBoostEnable":"1"}"#);
        worker.join().unwrap();
    }

    #[test]
    fn refused_subscription_is_not_treated_as_ready() {
        let listener = TcpListener::bind((BROKER_HOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read(&mut stream);
            stream.write_all(&packet(0x20, &[0, 0])).unwrap();
            read(&mut stream);
            stream
                .write_all(&packet(0x90, &[0, 1, 0x80, 0x80, 0x80]))
                .unwrap();
            // A failed handshake can close before DISCONNECT is delivered.
            let _ = next_packet(&mut stream, Instant::now() + Duration::from_secs(3));
        });
        let failure = BusClient::connect(addr)
            .err()
            .expect("subscription refused");
        assert!(
            failure.contains("refused a status subscription"),
            "{failure}"
        );
        worker.join().unwrap();
    }
}
