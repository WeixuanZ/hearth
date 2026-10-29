//! Samsung's local remote and art-mode messages.
//!
//! The Frame 2024 (LS03D, including the 65") speaks this on port 8002.
//! The first remote connection shows Allow on the TV and returns a token.
//! Art mode is a separate channel. On some 2024 firmware that channel stays
//! quiet, so the caller can fall back to the power key, which is what the
//! Frame's own remote does.

use serde_json::{json, Value};

pub const REMOTE_CHANNEL: &str = "samsung.remote.control";
pub const ART_CHANNEL: &str = "com.samsung.art-app";

/// The HDMI sockets. Art mode is a switch of its own, not an input. The ids
/// start at 2 because 1 was the art input, and Home remembers ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Hdmi1 = 2,
    Hdmi2 = 3,
    Hdmi3 = 4,
    Hdmi4 = 5,
}

impl Input {
    pub const ALL: [Self; 4] = [Self::Hdmi1, Self::Hdmi2, Self::Hdmi3, Self::Hdmi4];

    pub fn from_id(id: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|input| *input as u32 == id)
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Hdmi1 => "KEY_HDMI1",
            Self::Hdmi2 => "KEY_HDMI2",
            Self::Hdmi3 => "KEY_HDMI3",
            Self::Hdmi4 => "KEY_HDMI4",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hdmi1 => "HDMI 1",
            Self::Hdmi2 => "HDMI 2",
            Self::Hdmi3 => "HDMI 3",
            Self::Hdmi4 => "HDMI 4",
        }
    }

    /// HomeKit input source type: HDMI.
    pub fn source_type(self) -> u8 {
        3
    }
}

pub fn app_name() -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"Hearth")
}

pub fn channel_path(channel: &str, token: &str) -> String {
    let mut path = format!("/api/v2/channels/{channel}?name={}", app_name());
    if !token.is_empty() {
        path.push_str("&token=");
        path.push_str(token);
    }
    path
}

pub fn key_payload(key: &str) -> String {
    key_command("Click", key)
}

/// `Press` and `Release` hold a key down, as a long press on the remote.
pub fn key_command(cmd: &str, key: &str) -> String {
    json!({
        "method": "ms.remote.control",
        "params": {
            "Cmd": cmd,
            "DataOfCmd": key,
            "Option": "false",
            "TypeOfRemote": "SendRemoteKey"
        }
    })
    .to_string()
}

/// `id` is echoed back in the reply. Newer firmware ignores a request without one.
pub fn art_payload(request: &str, value: Option<&str>, id: &str) -> String {
    let mut data = serde_json::Map::new();
    data.insert("request".into(), json!(request));
    data.insert("id".into(), json!(id));
    data.insert("request_id".into(), json!(id));
    if let Some(value) = value {
        data.insert("value".into(), json!(value));
    }
    json!({
        "method": "ms.channel.emit",
        "params": {
            "event": "art_app_request",
            "to": "host",
            "data": Value::Object(data).to_string()
        }
    })
    .to_string()
}

/// Home's remote buttons, as Samsung key names.
pub fn hap_remote_key(value: u8) -> Option<&'static str> {
    Some(match value {
        0 => "KEY_REWIND",
        1 => "KEY_FF",
        4 => "KEY_UP",
        5 => "KEY_DOWN",
        6 => "KEY_LEFT",
        7 => "KEY_RIGHT",
        8 => "KEY_ENTER",
        9 => "KEY_RETURN",
        10 => "KEY_EXIT",
        11 => "KEY_PLAY",
        15 => "KEY_INFO",
        _ => return None,
    })
}

pub fn parse_token(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("event").and_then(|event| event.as_str()) != Some("ms.channel.connect") {
        return None;
    }
    let data = nested(value.get("data")?)?;
    data.get("token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub on: bool,
    pub mac: String,
    pub model: String,
    pub name: String,
    pub frame: bool,
}

pub fn parse_device_info(text: &str) -> Option<DeviceInfo> {
    let value: Value = serde_json::from_str(text).ok()?;
    let device = value.get("device").unwrap_or(&value);
    let power = device
        .get("PowerState")
        .and_then(Value::as_str)
        .unwrap_or("");
    let frame = device
        .get("FrameTVSupport")
        .map(|item| {
            item.as_str().unwrap_or("").eq_ignore_ascii_case("true") || item.as_bool() == Some(true)
        })
        .unwrap_or(false);
    Some(DeviceInfo {
        on: power.eq_ignore_ascii_case("on"),
        mac: device
            .get("wifiMac")
            .or_else(|| device.get("mac"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        model: device
            .get("modelName")
            .or_else(|| device.get("model"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        name: device
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        frame,
    })
}

/// Art-channel reply. `None` if this message is not about art mode or brightness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtReply {
    Mode(bool),
    Brightness(u8),
}

pub fn parse_art_message(text: &str) -> Option<ArtReply> {
    let value: Value = serde_json::from_str(text).ok()?;
    let data = nested(value.get("data").unwrap_or(&value))?;
    let data = nested(data.get("data").unwrap_or(&data)).unwrap_or(data);
    if let Some(mode) = data
        .get("value")
        .and_then(Value::as_str)
        .and_then(parse_mode)
    {
        return Some(ArtReply::Mode(mode));
    }
    if let Some(mode) = data
        .get("artmode")
        .or_else(|| data.get("art_mode"))
        .and_then(Value::as_str)
        .and_then(parse_mode)
    {
        return Some(ArtReply::Mode(mode));
    }
    // A 2024 Frame answers a set, and announces a change, with "status".
    let event = data.get("event").and_then(Value::as_str).unwrap_or("");
    if matches!(event, "set_artmode_status" | "art_mode_changed") {
        if let Some(mode) = data
            .get("status")
            .and_then(Value::as_str)
            .and_then(parse_mode)
        {
            return Some(ArtReply::Mode(mode));
        }
    }
    let brightness = data
        .get("brightness")
        .or_else(|| data.get("value"))
        .and_then(number);
    brightness.map(|level| ArtReply::Brightness(level.min(100)))
}

fn parse_mode(text: &str) -> Option<bool> {
    match text.to_ascii_lowercase().as_str() {
        "on" | "true" => Some(true),
        "off" | "false" => Some(false),
        _ => None,
    }
}

fn number(value: &Value) -> Option<u8> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .and_then(|n| u8::try_from(n).ok())
}

fn nested(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => serde_json::from_str(text).ok(),
        other => Some(other.clone()),
    }
}

/// The art API wants 0–10. HomeKit brightness is 0–100. A value already above 10 is a percent.
pub fn home_brightness(art: u8) -> i32 {
    if art > 10 {
        i32::from(art.min(100))
    } else {
        i32::from(art) * 10
    }
}

pub fn art_brightness(home: i32) -> u8 {
    home.clamp(0, 100) as u8 / 10
}

pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut packet = [0u8; 102];
    packet[..6].fill(0xff);
    for index in 0..16 {
        let start = 6 + index * 6;
        packet[start..start + 6].copy_from_slice(&mac);
    }
    packet
}

pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let hex: String = text
        .chars()
        .filter(|char| char.is_ascii_hexdigit())
        .collect();
    if hex.len() != 12 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (index, byte) in mac.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(mac)
}

pub fn location_host(response: &str) -> Option<String> {
    for line in response.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("LOCATION") {
            continue;
        }
        let value = value.trim();
        let rest = value.split("://").nth(1).unwrap_or(value);
        let host = rest.split([':', '/']).next()?.trim();
        if !host.is_empty() {
            return Some(host.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comes_from_the_connect_event() {
        let text = r#"{"event":"ms.channel.connect","data":{"token":"12345678","id":"abc"}}"#;
        assert_eq!(parse_token(text).as_deref(), Some("12345678"));
        let wrapped = r#"{"event":"ms.channel.connect","data":"{\"token\":\"99\"}"}"#;
        assert_eq!(parse_token(wrapped).as_deref(), Some("99"));
    }

    #[test]
    fn device_info_reads_power_and_mac() {
        let text = r#"{"device":{"PowerState":"on","FrameTVSupport":"true","wifiMac":"AA:BB:CC:DD:EE:FF","modelName":"QE65LS03D","name":"Frame"}}"#;
        let info = parse_device_info(text).unwrap();
        assert!(info.on && info.frame);
        assert_eq!(info.model, "QE65LS03D");
        assert_eq!(info.mac, "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn art_reply_accepts_a_nested_string() {
        let text = r#"{"event":"d2d_service_message","data":"{\"value\":\"on\"}"}"#;
        assert_eq!(parse_art_message(text), Some(ArtReply::Mode(true)));
        let brightness = r#"{"data":{"brightness":4}}"#;
        assert_eq!(parse_art_message(brightness), Some(ArtReply::Brightness(4)));
    }

    #[test]
    fn brightness_maps_between_home_and_the_art_scale() {
        assert_eq!(art_brightness(100), 10);
        assert_eq!(art_brightness(34), 3);
        assert_eq!(home_brightness(4), 40);
        assert_eq!(home_brightness(80), 80);
    }

    #[test]
    fn magic_packet_repeats_the_mac() {
        let mac = parse_mac("aa:bb:cc:dd:ee:ff").unwrap();
        let packet = magic_packet(mac);
        assert_eq!(&packet[..6], &[0xff; 6]);
        assert_eq!(&packet[6..12], &mac);
        assert_eq!(&packet[96..102], &mac);
    }

    #[test]
    fn ssdp_location_keeps_the_host() {
        let response = "HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.40:9197/dmr\r\n";
        assert_eq!(location_host(response).as_deref(), Some("192.168.1.40"));
    }

    #[test]
    fn art_reply_reads_status_from_a_set_or_a_change() {
        let set = r#"{"data":"{\n  \"event\": \"set_artmode_status\",\n  \"request_id\": \"abc\",\n  \"status\": \"on\"\n}","event":"d2d_service_message"}"#;
        assert_eq!(parse_art_message(set), Some(ArtReply::Mode(true)));
        let changed = r#"{"data":"{\"event\": \"art_mode_changed\", \"status\": \"off\"}","event":"d2d_service_message"}"#;
        assert_eq!(parse_art_message(changed), Some(ArtReply::Mode(false)));
        let image = r#"{"data":"{\"event\": \"image_selected\", \"is_shown\": \"Yes\"}","event":"d2d_service_message"}"#;
        assert_eq!(parse_art_message(image), None);
    }

    #[test]
    fn key_and_art_payloads_use_the_samsung_envelope() {
        let key = key_payload("KEY_POWEROFF");
        assert!(key.contains("SendRemoteKey"));
        assert!(key.contains("KEY_POWEROFF"));
        let art = art_payload("set_artmode_status", Some("on"), "abc");
        assert!(art.contains("art_app_request"));
        assert!(art.contains("set_artmode_status"));
        assert!(art.contains(r#"\"request_id\":\"abc\""#));
        assert_eq!(hap_remote_key(8), Some("KEY_ENTER"));
        assert_eq!(Input::from_id(1), None);
        assert_eq!(Input::from_id(3), Some(Input::Hdmi2));
        assert_eq!(Input::Hdmi2.key(), "KEY_HDMI2");
    }
}
