//! Samsung's local remote and art-mode messages.
//!
//! The Frame 2024 (LS03D, including the 65") speaks this on port 8002.
//! The first remote connection shows Allow on the TV and returns a token.
//! Art mode is a separate channel. On some 2024 firmware that channel stays
//! quiet, so the caller can fall back to the power key, which is what the
//! Frame's own remote does.

use serde_json::{json, Value};

pub const REMOTE_CHANNEL: &str = "samsung.remote.control";

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

/// Types `text` into the text box the TV shows, then ends the input, as a
/// phone's keyboard does. A TV with no text box open ignores both.
pub fn text_payloads(text: &str) -> [String; 2] {
    let typed = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, text);
    [
        json!({
            "method": "ms.remote.control",
            "params": {
                "Cmd": typed,
                "DataOfCmd": "base64",
                "TypeOfRemote": "SendInputString"
            }
        })
        .to_string(),
        json!({
            "method": "ms.remote.control",
            "params": { "TypeOfRemote": "SendInputEnd" }
        })
        .to_string(),
    ]
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

/// A Samsung message's field, which may hold its JSON as a string.
pub fn nested(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => serde_json::from_str(text).ok(),
        other => Some(other.clone()),
    }
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
    fn keys_use_the_samsung_envelope() {
        let key = key_payload("KEY_POWEROFF");
        assert!(key.contains("SendRemoteKey"));
        assert!(key.contains("KEY_POWEROFF"));
    }

    #[test]
    fn text_is_typed_then_ended() {
        let [typed, ended] = text_payloads("Hi there");
        assert!(typed.contains(r#""TypeOfRemote":"SendInputString""#));
        assert!(typed.contains(r#""Cmd":"SGkgdGhlcmU=""#), "{typed}");
        assert!(ended.contains(r#""TypeOfRemote":"SendInputEnd""#));
    }
}
