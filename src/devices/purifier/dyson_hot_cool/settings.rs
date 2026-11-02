//! What hearth keeps of a Dyson Hot+Cool purifier, in the config.

use crate::config::Settings;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DysonHotCoolDevice {
    pub name: String,
    pub serial: String,
    pub model: String,
    /// MQTT topic prefix. `auto` tries 527K, then 527E, then 527.
    pub product_type: String,
    pub mqtt_password: String,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_mqtt_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub heating: bool,
}

impl Settings for DysonHotCoolDevice {
    fn id(&self) -> String {
        format!("dyson:{}", self.serial)
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn name_mut(&mut self) -> &mut String {
        &mut self.name
    }
    /// Empty means found on the network by its serial.
    fn address(&self) -> Option<(String, u16)> {
        (!self.host.is_empty()).then(|| (self.host.clone(), self.port))
    }
}

fn default_mqtt_port() -> u16 {
    1883
}

fn default_true() -> bool {
    true
}
