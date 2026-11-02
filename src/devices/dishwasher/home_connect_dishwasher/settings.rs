//! What hearth keeps of a Bosch dishwasher, in the config.

use crate::{config::Settings, connectors::home_connect::protocol::Feature};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A dishwasher reached on the local network.
/// The account password is not stored. `key` is the machine's own key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeConnectDishwasherDevice {
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
    pub features: BTreeMap<String, Feature>,
}

fn default_brand() -> String {
    "Bosch".into()
}

impl Settings for HomeConnectDishwasherDevice {
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

impl HomeConnectDishwasherDevice {
    /// Where it is and how to talk to it, for a Home Connect session.
    pub fn appliance(&self) -> crate::connectors::home_connect::link::Appliance<'_> {
        crate::connectors::home_connect::link::Appliance {
            name: &self.name,
            host: &self.host,
            key: &self.key,
            iv: &self.iv,
            features: &self.features,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn washer() -> HomeConnectDishwasherDevice {
        HomeConnectDishwasherDevice {
            aid: 0,
            door_aid: 0,
            leak_aid: 0,
            cycle_aid: 0,
            name: "Bosch Dishwasher".into(),
            brand: "Bosch".into(),
            model: "XY01".into(),
            ha_id: "BOSCH-HA".into(),
            host: "BOSCH-Dishwasher-BOSCH-HA".into(),
            key: "secret".into(),
            iv: String::new(),
            features: BTreeMap::from([(
                "200".into(),
                Feature {
                    name: "BSH.Common.Setting.PowerState".into(),
                    values: BTreeMap::from([("0".into(), "Off".into()), ("1".into(), "On".into())]),
                },
            )]),
        }
    }

    #[test]
    fn a_bare_name_is_dialled_on_local() {
        let mut device = washer();
        let at = |device: &HomeConnectDishwasherDevice| {
            device
                .address()
                .map(|(host, port)| format!("{host}:{port}"))
        };
        assert_eq!(
            at(&device).as_deref(),
            Some("BOSCH-Dishwasher-BOSCH-HA.local:443")
        );
        device.host = "10.0.0.9".into();
        device.iv = "iv".into();
        assert_eq!(at(&device).as_deref(), Some("10.0.0.9:80"));
        device.host.clear();
        assert_eq!(at(&device), None);
    }

    #[test]
    fn its_settings_are_written_and_read_back_whole() {
        let text = toml::to_string(&washer()).unwrap();
        assert!(!text.contains("iv ="));
        let read: HomeConnectDishwasherDevice = toml::from_str(&text).unwrap();
        assert_eq!(read, washer());
        assert_eq!(
            read.features["200"].values.get("1").map(String::as_str),
            Some("On")
        );
    }
}
