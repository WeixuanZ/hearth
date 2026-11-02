//! What hearth keeps of a Samsung The Frame, in the config.

use crate::config::Settings;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FrameDevice {
    #[serde(default)]
    pub aid: u64,
    pub name: String,
    /// Filled in from the TV. The 65" 2024 Frame reports a model like QE65LS03D.
    #[serde(default)]
    pub model: String,
    pub host: String,
    #[serde(default)]
    pub mac: String,
    /// Returned when you press Allow on the TV. Empty until it is added.
    #[serde(default)]
    pub token: String,
    /// IP Control's token, from a second Allow. Empty if IP Remote is off.
    #[serde(default)]
    pub ip_token: String,
    /// HomeKit accessory id of the art switch, a tile beside the TV.
    #[serde(default)]
    pub art_aid: u64,
}

impl Settings for FrameDevice {
    fn id(&self) -> String {
        format!("frame:{}", self.host)
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn name_mut(&mut self) -> &mut String {
        &mut self.name
    }
    /// The TV, and art mode as a tile of its own.
    fn aids(&mut self) -> Vec<&mut u64> {
        vec![&mut self.aid, &mut self.art_aid]
    }
    /// The TV's own API, which answers even before it is paired.
    fn address(&self) -> Option<(String, u16)> {
        Some((self.host.clone(), 8001))
    }
    fn incomplete(&self) -> Option<&'static str> {
        self.token
            .is_empty()
            .then_some("not paired with the TV. Add it again while it is on")
    }
}
