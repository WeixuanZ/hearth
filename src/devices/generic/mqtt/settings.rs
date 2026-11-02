//! What hearth keeps of a device on an MQTT broker: where the broker is, and
//! for each of its controls, the topics it is read and set on.

use crate::{capabilities::Role, config::Settings};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MqttDevice {
    /// Its own name for good, like `desk-lamp`: hearth knows it as
    /// `mqtt:desk-lamp`, whatever it is named.
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub category: Category,
    /// The broker.
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    #[serde(default)]
    pub aid: u64,
    /// What it does, each as a capability.
    #[serde(default, rename = "control")]
    pub controls: Vec<Control>,
}

fn default_port() -> u16 {
    1883
}

/// What the device is, which picks how Home and the menu bar show it.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Light,
    #[default]
    Switch,
    Sensor,
    /// Anything else: its power as a switch, and its other controls.
    Other,
}

impl Category {
    pub fn word(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Switch => "switch",
            Self::Sensor => "sensor",
            Self::Other => "other",
        }
    }
}

/// One thing the device does or says.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Control {
    /// As in `hearth set lamp brightness 50`. A `power` control is always
    /// `power`.
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub kind: ControlKind,
    /// What it is, for Home: `brightness`, `temperature`, `humidity`,
    /// `pm25`, `contact`, `leak` and the like.
    #[serde(default, skip_serializing_if = "is_generic")]
    pub role: Role,
    /// The topic it reports its value on.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    /// The topic it is set on. Empty for one that is only read.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// The value is this field of a JSON message, like `temperature` or
    /// `state.on`, and is set by sending such a message. Empty for a
    /// message that is the value itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub field: String,
    /// What it says for on, for power, a switch or a state.
    #[serde(default = "on", skip_serializing_if = "is_on")]
    pub on: String,
    #[serde(default = "off", skip_serializing_if = "is_off")]
    pub off: String,
    /// The range of a level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unit: String,
    /// A choice's options.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<Choice>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    #[default]
    Power,
    Switch,
    Level,
    Reading,
    /// A yes or no it reports, like a door being open.
    State,
    Choice,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Choice {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// What the device says for it: its id if empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
}

impl Choice {
    pub fn said(&self) -> &str {
        if self.value.is_empty() {
            &self.id
        } else {
            &self.value
        }
    }
}

fn is_generic(role: &Role) -> bool {
    *role == Role::Generic
}

fn on() -> String {
    "ON".into()
}

fn off() -> String {
    "OFF".into()
}

fn is_on(text: &String) -> bool {
    *text == on()
}

fn is_off(text: &String) -> bool {
    *text == off()
}

impl Control {
    /// Its id as hearth knows it.
    pub fn id(&self) -> &str {
        if self.kind == ControlKind::Power {
            "power"
        } else {
            &self.id
        }
    }

    /// Its name, or its id made readable.
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        let words = self.id().replace(['_', '-'], " ");
        let mut letters = words.chars();
        letters
            .next()
            .map(|first| first.to_uppercase().chain(letters).collect())
            .unwrap_or_default()
    }
}

impl MqttDevice {
    /// Every topic its controls report on, once each.
    pub fn topics(&self) -> Vec<String> {
        let mut topics: Vec<String> = self
            .controls
            .iter()
            .map(|control| control.state.clone())
            .filter(|topic| !topic.is_empty())
            .collect();
        topics.sort();
        topics.dedup();
        topics
    }
}

impl Settings for MqttDevice {
    fn id(&self) -> String {
        format!("mqtt:{}", self.key)
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn name_mut(&mut self) -> &mut String {
        &mut self.name
    }
    fn aids(&mut self) -> Vec<&mut u64> {
        vec![&mut self.aid]
    }
    /// The broker.
    fn address(&self) -> Option<(String, u16)> {
        Some((self.host.clone(), self.port))
    }
    fn incomplete(&self) -> Option<&'static str> {
        self.controls
            .is_empty()
            .then_some("has no controls. Add them to its table in config.toml")
    }
}
