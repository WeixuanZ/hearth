//! The ids Home knows each device's accessories and services by.
//!
//! Home keeps rooms, scenes and automations by these ids, so once given they
//! never change: a device that gains a programme or loses an option keeps
//! every other id it had. They are kept in `homekit/ids.json`, which the
//! group shares with the bridge's keys. Anything new gets the next free id.
//! Devices know nothing of them: this is HomeKit's own record.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};

/// The bridge itself is accessory 1.
const FIRST_AID: u64 = 2;
/// The information service is 1; a device's own services start here.
const FIRST_IID: u64 = 20;
/// Room between services for their characteristics. hap numbers those after
/// the service's own id, and a television's reach 13 past it.
const SPAN: u64 = 50;

const FILE: &str = "ids.json";

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ids {
    #[serde(default)]
    devices: BTreeMap<String, DeviceIds>,
}

/// One device's ids.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceIds {
    /// Accessories by what they are: `main`, or a tile of its own, like `art`.
    #[serde(default)]
    pub accessories: BTreeMap<String, u64>,
    /// Services by accessory, then by what they are, like `switch:night`.
    #[serde(default)]
    pub services: BTreeMap<String, BTreeMap<String, u64>>,
}

impl Ids {
    pub fn load(dir: &Path) -> Result<Self> {
        match fs::read_to_string(dir.join(FILE)) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        crate::files::write_json(&dir.join(FILE), self)
    }

    /// Takes the ids a device had before this store, once: a device the
    /// store knows keeps what it has. Every device is seeded before any new
    /// id is given, so a new one cannot land on an old one. Nothing is ever
    /// forgotten, so no id is given twice, even after a device is removed.
    #[cfg(test)]
    pub fn seed(&mut self, device: &str, seed: impl FnOnce() -> DeviceIds) {
        self.devices.entry(device.to_owned()).or_insert_with(seed);
    }

    /// The id of `device`'s accessory `key`, the next free one if new.
    pub fn aid(&mut self, device: &str, key: &str) -> u64 {
        if let Some(aid) = self
            .devices
            .get(device)
            .and_then(|ids| ids.accessories.get(key))
        {
            return *aid;
        }
        let next = self
            .devices
            .values()
            .flat_map(|ids| ids.accessories.values())
            .max()
            .map_or(FIRST_AID, |last| last + 1)
            .max(FIRST_AID);
        let ids = self.devices.entry(device.to_owned()).or_default();
        ids.accessories.insert(key.to_owned(), next);
        next
    }

    /// The id of service `key` in `device`'s accessory `accessory`, the next
    /// free one, well past the last, if new.
    pub fn iid(&mut self, device: &str, accessory: &str, key: &str) -> u64 {
        let services = self
            .devices
            .entry(device.to_owned())
            .or_default()
            .services
            .entry(accessory.to_owned())
            .or_default();
        if let Some(iid) = services.get(key) {
            return *iid;
        }
        let next = services
            .values()
            .max()
            .map_or(FIRST_IID, |last| (last + SPAN).div_ceil(10) * 10);
        services.insert(key.to_owned(), next);
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_once_given_never_change_and_new_ones_do_not_collide() {
        let mut ids = Ids::default();
        ids.seed("tv:1", || DeviceIds {
            accessories: BTreeMap::from([("main".into(), 2), ("art".into(), 4)]),
            services: BTreeMap::from([(
                "main".into(),
                BTreeMap::from([("television".into(), 20), ("input:tv".into(), 90)]),
            )]),
        });
        // A second seed does not move what is there.
        ids.seed("tv:1", DeviceIds::default);
        assert_eq!(ids.aid("tv:1", "art"), 4);
        assert_eq!(ids.iid("tv:1", "main", "television"), 20);
        // New: past the highest, and stable after.
        assert_eq!(ids.aid("fan:1", "main"), 5);
        assert_eq!(ids.aid("fan:1", "main"), 5);
        assert_eq!(ids.iid("tv:1", "main", "speaker"), 140);
        assert_eq!(ids.iid("fan:1", "main", "purifier"), FIRST_IID);
        assert_eq!(ids.iid("fan:1", "main", "air"), FIRST_IID + SPAN);
    }

    #[test]
    fn the_store_survives_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("hearth-ids-{}", std::process::id()));
        let mut ids = Ids::default();
        ids.aid("a:1", "main");
        ids.iid("a:1", "main", "switch:night");
        ids.save(&dir).unwrap();
        assert_eq!(Ids::load(&dir).unwrap(), ids);
        assert_eq!(Ids::load(&dir.join("none")).unwrap(), Ids::default());
        let _ = fs::remove_dir_all(dir);
    }
}
