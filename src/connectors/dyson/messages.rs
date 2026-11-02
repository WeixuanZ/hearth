//! What every Dyson fan on MQTT shares: where it listens and reports, and
//! how a message asking for its state, or setting it, is framed. What the
//! fields mean is each model's (see the purifier models under `devices`).

use serde_json::{Map, Value};

pub fn state_set_payload(fields: &[(&str, String)]) -> String {
    let mut data = Map::new();
    for (key, value) in fields {
        data.insert((*key).to_owned(), Value::String(value.clone()));
    }
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    serde_json::json!({
        "msg": "STATE-SET",
        "time": now,
        "mode-reason": "LAPP",
        "data": data,
    })
    .to_string()
}

pub fn request_state_payload() -> String {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    serde_json::json!({ "msg": "REQUEST-CURRENT-STATE", "time": now }).to_string()
}

pub fn request_environment_payload() -> String {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    serde_json::json!({
        "msg": "REQUEST-PRODUCT-ENVIRONMENT-CURRENT-SENSOR-DATA",
        "time": now,
    })
    .to_string()
}

pub fn status_topic(product_type: &str, serial: &str) -> String {
    format!("{product_type}/{serial}/status/current")
}

pub fn command_topic(product_type: &str, serial: &str) -> String {
    format!("{product_type}/{serial}/command")
}
