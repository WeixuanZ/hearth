//! What hearth keeps of a Samsung The Frame, in the config.

use crate::config::Settings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SamsungFrameDevice {
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
    /// Each computer's own token, by its id (`config::computer_id`). The TV
    /// allows each computer by its address and gives it a token of its own,
    /// and asks again when one shows another's. In a group, one shared token
    /// would be replaced by each computer that serves, and the TV would ask
    /// every time Home moved.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tokens: BTreeMap<String, String>,
    /// IP Control's token, from a second Allow. Empty if IP Remote is off.
    #[serde(default)]
    pub ip_token: String,
}

impl SamsungFrameDevice {
    /// The token this computer shows the TV: its own, or else the one the
    /// TV gave when it was added.
    pub fn token_for(&self, computer: &str) -> &str {
        self.tokens
            .get(computer)
            .filter(|token| !token.is_empty())
            .unwrap_or(&self.token)
    }
}

impl Settings for SamsungFrameDevice {
    fn id(&self) -> String {
        format!("frame:{}", self.host)
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn name_mut(&mut self) -> &mut String {
        &mut self.name
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_computer_shows_the_tv_its_own_token() {
        let device = SamsungFrameDevice {
            token: "added".into(),
            tokens: [("b".to_owned(), "b's".to_owned())].into(),
            ..Default::default()
        };
        assert_eq!(device.token_for("a"), "added");
        assert_eq!(device.token_for("b"), "b's");
    }
}
