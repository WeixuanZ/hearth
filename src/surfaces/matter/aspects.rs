//! What of each device Matter shows, and the endpoint each has for good.
//!
//! A device is one or more bridged endpoints, one per thing a controller
//! knows: its power as a plug, each other switch as a plug of its own, and
//! its temperature, humidity, door and leak as sensors. A machine that runs
//! programmes shows only its sensors: on/off is too easy to set from a
//! controller to start a dishwasher with.

use crate::capabilities::{Kind, Role, Snapshot};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

/// What an endpoint is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aspect {
    /// On and off, of the capability with this id.
    Switch(&'static str),
    Temperature(&'static str),
    Humidity(&'static str),
    /// Open or closed, of a capability that is true while open.
    Contact(&'static str),
    Leak(&'static str),
}

impl Aspect {
    pub fn capability(self) -> &'static str {
        match self {
            Self::Switch(id)
            | Self::Temperature(id)
            | Self::Humidity(id)
            | Self::Contact(id)
            | Self::Leak(id) => id,
        }
    }
}

/// One bridged endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct Bridged {
    pub endpoint: u16,
    pub device: String,
    /// What its name adds to the device's, as " Art mode" in "The Frame
    /// Art mode". Empty for the device's own power.
    pub suffix: String,
    pub aspect: Aspect,
}

/// What of `snapshot` Matter shows, each with a key that names it for good
/// and what its name adds to the device's.
pub fn of(snapshot: &Snapshot) -> Vec<(String, String, Aspect)> {
    let machine = snapshot
        .capabilities
        .iter()
        .any(|capability| matches!(capability.kind, Kind::Run { .. }));
    let mut found = Vec::new();
    for capability in &snapshot.capabilities {
        let id = capability.id;
        let labelled = |what: &str| format!(" {what}");
        let aspect = match (&capability.kind, capability.role) {
            (Kind::Power, _) if !machine => Some((String::new(), Aspect::Switch(id))),
            (Kind::Switch, Role::Generic) if !machine => {
                Some((labelled(&capability.name), Aspect::Switch(id)))
            }
            (_, Role::Temperature) => Some((labelled("temperature"), Aspect::Temperature(id))),
            (_, Role::Humidity) => Some((labelled("humidity"), Aspect::Humidity(id))),
            (Kind::State, Role::Contact) => Some((labelled("door"), Aspect::Contact(id))),
            (Kind::State, Role::Leak) => Some((labelled("leak"), Aspect::Leak(id))),
            _ => None,
        };
        if let Some((label, aspect)) = aspect {
            found.push((id.to_owned(), label, aspect));
        }
    }
    found
}

/// Each endpoint's id, by device and aspect, kept so a controller's rooms
/// and automations stay with them whatever is added later. 0 is the root
/// and 1 the aggregator; bridged endpoints start at 2, and a new one takes
/// the next never used.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Endpoints {
    ids: BTreeMap<String, u16>,
}

const FILE: &str = "endpoints.json";

impl Endpoints {
    pub fn load(dir: &Path) -> Result<Self> {
        match std::fs::read_to_string(dir.join(FILE)) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(_) => Ok(Self::default()),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let next = dir.join(format!("{FILE}.next"));
        std::fs::write(&next, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(next, dir.join(FILE))?;
        Ok(())
    }

    pub fn id(&mut self, device: &str, key: &str) -> u16 {
        let name = format!("{device}/{key}");
        if let Some(id) = self.ids.get(&name) {
            return *id;
        }
        let id = self.ids.values().max().map_or(2, |most| most + 1);
        self.ids.insert(name, id);
        id
    }
}

/// Every bridged endpoint for `snapshots`, with ids from `endpoints`.
pub fn bridged(snapshots: &[Snapshot], endpoints: &mut Endpoints) -> Vec<Bridged> {
    let mut all = Vec::new();
    for snapshot in snapshots {
        for (key, suffix, aspect) in of(snapshot) {
            all.push(Bridged {
                endpoint: endpoints.id(&snapshot.id, &key),
                device: snapshot.id.clone(),
                suffix,
                aspect,
            });
        }
    }
    all.sort_by_key(|bridged| bridged.endpoint);
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{Capability, Measure};

    fn snapshot(id: &str, name: &str, capabilities: Vec<Capability>) -> Snapshot {
        Snapshot {
            id: id.into(),
            kind: "test",
            category: "other",
            name: name.into(),
            online: true,
            headline: String::new(),
            detail: String::new(),
            capabilities,
            alerts: Vec::new(),
            about: Default::default(),
        }
    }

    #[test]
    fn a_device_is_its_power_switches_and_sensors() {
        let fan = snapshot(
            "fan",
            "Fan",
            vec![
                Capability::power(true),
                Capability::switch("night", "Night mode", false),
                Capability::measure(Measure::Temperature, Some(21.0)),
                Capability::level("speed", "Speed", (1.0, 10.0, 1.0), "", Some(4.0)),
            ],
        );
        let suffixes: Vec<(String, Aspect)> = of(&fan)
            .into_iter()
            .map(|(_, suffix, aspect)| (suffix, aspect))
            .collect();
        assert_eq!(
            suffixes,
            [
                (String::new(), Aspect::Switch("power")),
                (" Night mode".to_owned(), Aspect::Switch("night")),
                (
                    " temperature".to_owned(),
                    Aspect::Temperature("temperature")
                ),
            ]
        );
    }

    #[test]
    fn a_machine_that_runs_programmes_shows_only_its_sensors() {
        let dishwasher = snapshot(
            "dishwasher",
            "Dishwasher",
            vec![
                Capability::run(false, None, None, None),
                Capability::switch("extra_dry", "Extra dry", false),
                Capability::state("door", "Door open", Some(false)).role(Role::Contact),
                Capability::state("leak", "Leak", Some(false)).role(Role::Leak),
            ],
        );
        let aspects: Vec<Aspect> = of(&dishwasher)
            .into_iter()
            .map(|(_, _, aspect)| aspect)
            .collect();
        assert_eq!(aspects, [Aspect::Contact("door"), Aspect::Leak("leak")]);
    }

    #[test]
    fn endpoints_keep_their_ids_and_a_new_one_takes_the_next() {
        let dir = std::env::temp_dir().join(format!("hearth-matter-{}", rand::random::<u64>()));
        let tv = snapshot("tv", "TV", vec![Capability::power(false)]);
        let mut endpoints = Endpoints::default();
        let first = bridged(std::slice::from_ref(&tv), &mut endpoints);
        assert_eq!(first[0].endpoint, 2);
        endpoints.save(&dir).unwrap();
        // A fan added before it in the list still comes after it.
        let fan = snapshot("fan", "Fan", vec![Capability::power(false)]);
        let mut endpoints = Endpoints::load(&dir).unwrap();
        let again = bridged(&[fan, tv], &mut endpoints);
        let ids: Vec<(&str, u16)> = again
            .iter()
            .map(|b| (b.device.as_str(), b.endpoint))
            .collect();
        assert_eq!(ids, [("tv", 2), ("fan", 3)]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
