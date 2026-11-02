//! The Frame's own messages: its art channel, and the inputs it has. What
//! every Samsung TV shares (the remote, pairing, IP Control) is in
//! `connectors::samsung`.

use crate::connectors::samsung::protocol::nested;
use serde_json::{json, Value};

pub const ART_CHANNEL: &str = "com.samsung.art-app";

/// The TV's sources. Art mode is a switch of its own, not an input. The ids
/// start at 2 because 1 was the art input, and Home remembers ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Hdmi1 = 2,
    Hdmi2 = 3,
    Hdmi3 = 4,
    Hdmi4 = 5,
    /// Samsung's own source: the tuner and the apps.
    Tv = 6,
}

impl Input {
    pub const ALL: [Self; 5] = [Self::Tv, Self::Hdmi1, Self::Hdmi2, Self::Hdmi3, Self::Hdmi4];

    pub fn from_id(id: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|input| *input as u32 == id)
    }

    /// From IP Control's name for it.
    pub fn from_source(source: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|input| input.source() == source)
    }

    /// IP Control's name for it.
    pub fn source(self) -> &'static str {
        match self {
            Self::Hdmi1 => "HDMI1",
            Self::Hdmi2 => "HDMI2",
            Self::Hdmi3 => "HDMI3",
            Self::Hdmi4 => "HDMI4",
            Self::Tv => "TV",
        }
    }

    /// The remote key, for a TV without IP Control. Newer firmware may ignore it.
    pub fn key(self) -> String {
        format!("KEY_{}", self.source())
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hdmi1 => "HDMI 1",
            Self::Hdmi2 => "HDMI 2",
            Self::Hdmi3 => "HDMI 3",
            Self::Hdmi4 => "HDMI 4",
            Self::Tv => "TV",
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn art_reply_reads_status_from_a_set_or_a_change() {
        let set = r#"{"data":"{\n  \"event\": \"set_artmode_status\",\n  \"request_id\": \"abc\",\n  \"status\": \"on\"\n}","event":"d2d_service_message"}"#;
        assert_eq!(parse_art_message(set), Some(ArtReply::Mode(true)));
        let changed = r#"{"data":"{\"event\": \"art_mode_changed\", \"status\": \"off\"}","event":"d2d_service_message"}"#;
        assert_eq!(parse_art_message(changed), Some(ArtReply::Mode(false)));
        let image = r#"{"data":"{\"event\": \"image_selected\", \"is_shown\": \"Yes\"}","event":"d2d_service_message"}"#;
        assert_eq!(parse_art_message(image), None);
    }

    #[test]
    fn art_requests_and_inputs() {
        let art = art_payload("set_artmode_status", Some("on"), "abc");
        assert!(art.contains("art_app_request"));
        assert!(art.contains("set_artmode_status"));
        assert!(art.contains(r#"\"request_id\":\"abc\""#));
        assert_eq!(Input::from_id(1), None);
        assert_eq!(Input::from_id(3), Some(Input::Hdmi2));
        assert_eq!(Input::Hdmi2.key(), "KEY_HDMI2");
        assert_eq!(Input::from_source("HDMI4"), Some(Input::Hdmi4));
        assert_eq!(Input::from_source("TV"), Some(Input::Tv));
    }
}
