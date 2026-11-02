//! A configured MQTT device as capabilities: what each message it sends
//! means, and the message each request comes to.

use super::settings::{Control, ControlKind, MqttDevice};
use crate::{
    capabilities::{intern, About, Capability, Item, Set, Snapshot},
    config::Settings as _,
    connectors::mqtt::Publish,
};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// The device as hearth last heard it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// The broker took the session. Until then, and between sessions, the
    /// values are what it last said, not what it is.
    pub connected: bool,
    /// Each control's value, by its id.
    pub values: BTreeMap<String, Value>,
}

pub fn snapshot(device: &MqttDevice, state: &State) -> Snapshot {
    let capabilities: Vec<Capability> = device
        .controls
        .iter()
        .map(|control| capability(control, state.values.get(control.id())))
        .collect();
    let headline = if !state.connected {
        "Connecting".to_owned()
    } else if let Some(power) = state.values.get("power").and_then(Value::as_bool) {
        if power { "On" } else { "Off" }.to_owned()
    } else {
        // What it measures first, as a sensor shows it.
        device
            .controls
            .iter()
            .filter(|control| control.kind == ControlKind::Reading)
            .find_map(|control| {
                let value = state.values.get(control.id())?.as_f64()?;
                Some(format!("{value} {}", control.unit).trim_end().to_owned())
            })
            .unwrap_or_else(|| "Connected".into())
    };
    Snapshot {
        id: device.id(),
        kind: "mqtt",
        category: device.category.word(),
        name: device.name.clone(),
        online: state.connected,
        headline,
        detail: format!("MQTT {}", device.category.word()),
        capabilities,
        alerts: Vec::new(),
        about: About {
            maker: "MQTT".into(),
            model: device.category.word().into(),
            serial: device.key.clone(),
            firmware: String::new(),
        },
    }
}

fn capability(control: &Control, value: Option<&Value>) -> Capability {
    let id = intern(control.id());
    let name = control.label();
    let unit = intern(&control.unit);
    let number = value.and_then(Value::as_f64).map(|value| value as f32);
    let on = value.and_then(Value::as_bool);
    let capability = match control.kind {
        ControlKind::Power => Capability::power(on.unwrap_or(false)),
        ControlKind::Switch => Capability::switch(id, &name, on.unwrap_or(false)),
        ControlKind::State => Capability::state(id, &name, on),
        ControlKind::Reading => Capability::reading(id, &name, unit, number),
        ControlKind::Level => Capability::level(
            id,
            &name,
            (
                control.min.unwrap_or(0.0),
                control.max.unwrap_or(100.0),
                control.step.unwrap_or(1.0),
            ),
            unit,
            number,
        ),
        ControlKind::Choice => Capability::choice(
            id,
            &name,
            control
                .options
                .iter()
                .map(|option| {
                    let name = if option.name.is_empty() {
                        &option.id
                    } else {
                        &option.name
                    };
                    Item::new(option.id.clone(), name.clone())
                })
                .collect(),
            value.and_then(Value::as_str).map(str::to_owned),
        ),
    };
    capability.role(control.role)
}

/// What a message on `topic` says, kept in `state`. Whether anything
/// changed.
pub fn heard(device: &MqttDevice, state: &mut State, topic: &str, payload: &str) -> bool {
    let mut changed = false;
    for control in device
        .controls
        .iter()
        .filter(|control| control.state == topic)
    {
        let Some(value) = read(control, payload) else {
            continue;
        };
        if state.values.get(control.id()) != Some(&value) {
            state.values.insert(control.id().to_owned(), value);
            changed = true;
        }
    }
    changed
}

/// The value `control` has in `payload`: a bool for power, a switch or a
/// state; a number for a level or a reading; an option id for a choice.
/// None for a message that does not say.
pub fn read(control: &Control, payload: &str) -> Option<Value> {
    let said = said(&control.field, payload)?;
    let text = match &said {
        Value::String(text) => text.trim().to_owned(),
        other => other.to_string(),
    };
    match control.kind {
        ControlKind::Power | ControlKind::Switch | ControlKind::State => {
            if let Value::Bool(on) = said {
                return Some(json!(on));
            }
            if text.eq_ignore_ascii_case(&control.on) {
                Some(json!(true))
            } else if text.eq_ignore_ascii_case(&control.off) {
                Some(json!(false))
            } else {
                match text.to_ascii_lowercase().as_str() {
                    "true" | "1" | "on" => Some(json!(true)),
                    "false" | "0" | "off" => Some(json!(false)),
                    _ => None,
                }
            }
        }
        ControlKind::Level | ControlKind::Reading => {
            let number = said.as_f64().or_else(|| text.parse().ok())?;
            number.is_finite().then(|| json!(number))
        }
        ControlKind::Choice => control
            .options
            .iter()
            .find(|option| option.said() == text)
            .map(|option| json!(option.id)),
    }
}

/// `payload`, or its `field` (a dotted path) when it is JSON.
fn said(field: &str, payload: &str) -> Option<Value> {
    if field.is_empty() {
        return Some(
            serde_json::from_str::<Value>(payload)
                .ok()
                .filter(|value| !value.is_object() && !value.is_array())
                .unwrap_or_else(|| json!(payload)),
        );
    }
    let mut value = serde_json::from_str::<Value>(payload).ok()?;
    for part in field.split('.') {
        value = value.get_mut(part)?.take();
    }
    (!value.is_null()).then_some(value)
}

/// What a request comes to: the message to send, and the value to show
/// until the device says otherwise.
pub fn command(device: &MqttDevice, set: &Set) -> Result<(Publish, String, Value)> {
    let control = device
        .controls
        .iter()
        .find(|control| control.id() == set.capability)
        .ok_or_else(|| anyhow!("{} has no {}", device.name, set.capability))?;
    if control.command.is_empty() {
        bail!("{} is only read, not set", control.label());
    }
    let (value, shown) = match control.kind {
        ControlKind::Power | ControlKind::Switch => {
            let on = set.bool()?;
            let word = if on { &control.on } else { &control.off };
            (json!(word), json!(on))
        }
        ControlKind::Level => {
            let number = set.number()?;
            let whole = number.fract() == 0.0;
            let value = if whole {
                json!(number as i64)
            } else {
                json!(number)
            };
            (value, json!(number))
        }
        ControlKind::Choice => {
            let id = set
                .value
                .as_str()
                .ok_or_else(|| anyhow!("{} takes an option", control.label()))?;
            let option = control
                .options
                .iter()
                .find(|option| option.id == id)
                .ok_or_else(|| anyhow!("{} has no option {id}", control.label()))?;
            (json!(option.said()), json!(option.id))
        }
        ControlKind::Reading | ControlKind::State => {
            bail!("{} is only read, not set", control.label())
        }
    };
    let payload = if control.field.is_empty() {
        match value {
            Value::String(text) => text,
            other => other.to_string(),
        }
    } else {
        nested(&control.field, value).to_string()
    };
    let publish = Publish {
        topic: control.command.clone(),
        payload,
    };
    Ok((publish, control.id().to_owned(), shown))
}

/// `{"a": {"b": value}}` for the field `a.b`.
fn nested(field: &str, value: Value) -> Value {
    field.rsplit('.').fold(value, |inner, part| {
        let mut object = Map::new();
        object.insert(part.to_owned(), inner);
        Value::Object(object)
    })
}

#[cfg(test)]
mod tests {
    use super::super::settings::{Category, Choice};
    use super::*;
    use crate::capabilities::Role;

    fn lamp() -> MqttDevice {
        MqttDevice {
            key: "desk-lamp".into(),
            name: "Desk lamp".into(),
            category: Category::Light,
            host: "10.0.0.5".into(),
            port: 1883,
            controls: vec![
                Control {
                    kind: ControlKind::Power,
                    state: "lamp/state".into(),
                    command: "lamp/set".into(),
                    field: "state".into(),
                    on: "ON".into(),
                    off: "OFF".into(),
                    ..Control::default()
                },
                Control {
                    id: "brightness".into(),
                    kind: ControlKind::Level,
                    role: Role::Brightness,
                    state: "lamp/state".into(),
                    command: "lamp/set".into(),
                    field: "brightness".into(),
                    max: Some(254.0),
                    on: "ON".into(),
                    off: "OFF".into(),
                    ..Control::default()
                },
                Control {
                    id: "effect".into(),
                    kind: ControlKind::Choice,
                    state: "lamp/effect".into(),
                    command: "lamp/effect/set".into(),
                    options: vec![
                        Choice {
                            id: "warm".into(),
                            name: "Warm".into(),
                            value: "2700".into(),
                        },
                        Choice {
                            id: "cool".into(),
                            name: String::new(),
                            value: "5000".into(),
                        },
                    ],
                    on: "ON".into(),
                    off: "OFF".into(),
                    ..Control::default()
                },
            ],
            ..MqttDevice::default()
        }
    }

    #[test]
    fn a_json_message_sets_every_control_it_names() {
        let device = lamp();
        let mut state = State::default();
        assert!(heard(
            &device,
            &mut state,
            "lamp/state",
            r#"{"state":"ON","brightness":127}"#
        ));
        assert_eq!(state.values["power"], json!(true));
        assert_eq!(state.values["brightness"], json!(127.0));
        // The same again changes nothing.
        assert!(!heard(
            &device,
            &mut state,
            "lamp/state",
            r#"{"state":"ON","brightness":127}"#
        ));
        assert!(heard(&device, &mut state, "lamp/effect", "5000"));
        assert_eq!(state.values["effect"], json!("cool"));
        // A message that says nothing it knows is left alone.
        assert!(!heard(
            &device,
            &mut state,
            "lamp/state",
            r#"{"linkquality":90}"#
        ));
        assert!(!heard(&device, &mut state, "elsewhere", "ON"));
    }

    #[test]
    fn plain_messages_read_as_their_kind() {
        let switch = Control {
            kind: ControlKind::Switch,
            on: "1".into(),
            off: "0".into(),
            ..Control::default()
        };
        assert_eq!(read(&switch, "1"), Some(json!(true)));
        assert_eq!(read(&switch, "true"), Some(json!(true)));
        assert_eq!(read(&switch, "0"), Some(json!(false)));
        assert_eq!(read(&switch, "maybe"), None);
        let reading = Control {
            kind: ControlKind::Reading,
            ..Control::default()
        };
        assert_eq!(read(&reading, " 21.5 "), Some(json!(21.5)));
        assert_eq!(read(&reading, "warm"), None);
    }

    #[test]
    fn a_request_is_the_message_the_device_expects() {
        let device = lamp();
        let (publish, id, shown) = command(&device, &Set::test("power", json!(true))).unwrap();
        assert_eq!(
            (publish.topic.as_str(), publish.payload.as_str()),
            ("lamp/set", r#"{"state":"ON"}"#)
        );
        assert_eq!((id.as_str(), shown), ("power", json!(true)));
        let (publish, _, shown) = command(&device, &Set::test("brightness", json!(200))).unwrap();
        assert_eq!(publish.payload, r#"{"brightness":200}"#);
        assert_eq!(shown, json!(200.0));
        let (publish, _, _) = command(&device, &Set::test("effect", json!("warm"))).unwrap();
        assert_eq!(
            (publish.topic.as_str(), publish.payload.as_str()),
            ("lamp/effect/set", "2700")
        );
        assert!(command(&device, &Set::test("colour", json!(1))).is_err());
    }

    #[test]
    fn its_snapshot_is_what_it_said() {
        let device = lamp();
        let mut state = State::default();
        assert_eq!(snapshot(&device, &state).headline, "Connecting");
        state.connected = true;
        heard(
            &device,
            &mut state,
            "lamp/state",
            r#"{"state":"OFF","brightness":10}"#,
        );
        let shown = snapshot(&device, &state);
        assert_eq!(
            (shown.id.as_str(), shown.category, shown.headline.as_str()),
            ("mqtt:desk-lamp", "light", "Off")
        );
        let ids: Vec<&str> = shown
            .capabilities
            .iter()
            .map(|capability| capability.id)
            .collect();
        assert_eq!(ids, ["power", "brightness", "effect"]);
        assert_eq!(shown.capabilities[1].role, Role::Brightness);
    }
}
