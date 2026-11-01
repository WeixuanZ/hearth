//! What hearth keeps of a Bosch dishwasher, in the config.

use crate::{config::Settings, connectors::bosch::protocol::BoschFeature};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A dishwasher reached on the local network.
/// The account password is not stored. `key` is the machine's own key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BoschDevice {
    #[serde(default)]
    pub aid: u64,
    /// Door, AquaStop and the end of a cycle. Separate accessories, so Home can notify.
    #[serde(default)]
    pub door_aid: u64,
    #[serde(default)]
    pub leak_aid: u64,
    #[serde(default)]
    pub cycle_aid: u64,
    pub name: String,
    #[serde(default = "default_brand")]
    pub brand: String,
    /// VIB, such as SPV2HKX42G.
    #[serde(default)]
    pub model: String,
    pub ha_id: String,
    /// mDNS name, or an address passed with `--host`.
    #[serde(default)]
    pub host: String,
    pub key: String,
    /// Empty means TLS-PSK on 443. Set means the older AES session on port 80.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub iv: String,
    #[serde(default)]
    pub features: BTreeMap<String, BoschFeature>,
}

fn default_brand() -> String {
    "Bosch".into()
}

impl Settings for BoschDevice {
    fn id(&self) -> String {
        format!("bosch:{}", self.ha_id)
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn name_mut(&mut self) -> &mut String {
        &mut self.name
    }
    fn aids(&mut self) -> Vec<&mut u64> {
        vec![
            &mut self.aid,
            &mut self.door_aid,
            &mut self.leak_aid,
            &mut self.cycle_aid,
        ]
    }
    /// The machine announces a bare name, which resolves with `.local`.
    fn address(&self) -> Option<(String, u16)> {
        let port = if self.iv.is_empty() { 443 } else { 80 };
        let host = match self.host.contains('.') || self.host.contains(':') {
            true => self.host.clone(),
            false => format!("{}.local", self.host),
        };
        (!self.host.is_empty()).then_some((host, port))
    }
}
