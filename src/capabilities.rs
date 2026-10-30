//! Devices as every surface sees them: what they can do, and how to ask.
//!
//! A connector publishes a [`Snapshot`] of each device whenever it changes,
//! and listens for [`Set`]s on a channel. HomeKit, the menu bar and the CLI
//! all read the same snapshots and send to the same channels, so they agree,
//! and a new device needs no new UI: its capabilities are the UI.

use crate::config::{self, Config};
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Mutex;
use tokio::sync::{mpsc, oneshot, watch};

/// One thing a device can do or report.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Capability {
    /// Stable, as in `hearth set fan speed 4`.
    pub id: &'static str,
    pub name: String,
    #[serde(flatten)]
    pub kind: Kind,
    /// A bool for power and switches, an option id for a choice, a number
    /// for a level or reading. Null when not known yet.
    pub value: Value,
    /// False when the device is not measuring, or the control does nothing
    /// now. Surfaces grey it out.
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    /// The device's main on and off.
    Power,
    Switch,
    /// One of a few options, like an input.
    Choice {
        options: Vec<Item>,
    },
    /// A number the user sets, like a speed.
    Level {
        min: f32,
        max: f32,
        step: f32,
        unit: &'static str,
    },
    /// A number the device reports.
    Reading {
        unit: &'static str,
    },
    /// Momentary buttons, like a remote. Setting one presses it.
    Keys {
        keys: Vec<Item>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Item {
    pub id: String,
    pub name: String,
}

impl Item {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }
}

impl Capability {
    pub fn power(on: bool) -> Self {
        Self::new("power", "Power", Kind::Power, json!(on))
    }

    pub fn switch(id: &'static str, name: &str, on: bool) -> Self {
        Self::new(id, name, Kind::Switch, json!(on))
    }

    pub fn choice(id: &'static str, name: &str, options: Vec<Item>, value: Option<String>) -> Self {
        Self::new(id, name, Kind::Choice { options }, json!(value))
    }

    pub fn level(
        id: &'static str,
        name: &str,
        range: (f32, f32, f32),
        unit: &'static str,
        value: Option<f32>,
    ) -> Self {
        let (min, max, step) = range;
        Self::new(
            id,
            name,
            Kind::Level {
                min,
                max,
                step,
                unit,
            },
            json!(value),
        )
    }

    pub fn reading(id: &'static str, name: &str, unit: &'static str, value: Option<f32>) -> Self {
        let active = value.is_some();
        Self::new(id, name, Kind::Reading { unit }, json!(value)).active(active)
    }

    pub fn keys(id: &'static str, name: &str, keys: Vec<Item>) -> Self {
        Self::new(id, name, Kind::Keys { keys }, Value::Null)
    }

    fn new(id: &'static str, name: &str, kind: Kind, value: Value) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            value,
            active: true,
        }
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
}

/// A device as the menu bar draws it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Snapshot {
    pub id: String,
    /// The connector, like `dyson`.
    pub kind: &'static str,
    /// What it is, like `tv`. Picks the icon, and names it in the CLI.
    pub category: &'static str,
    pub name: String,
    pub online: bool,
    pub headline: String,
    pub detail: String,
    pub capabilities: Vec<Capability>,
}

/// A request to change one capability. The device answers through `done`
/// once it has acted, so a refusal reaches whoever asked.
#[derive(Debug)]
pub struct Set {
    pub capability: String,
    pub value: Value,
    done: oneshot::Sender<Result<()>>,
}

impl Set {
    pub fn finish(self, result: Result<()>) {
        let _ = self.done.send(result);
    }

    pub fn bool(&self) -> Result<bool> {
        self.value
            .as_bool()
            .ok_or_else(|| anyhow!("{} takes true or false", self.capability))
    }

    pub fn number(&self) -> Result<f32> {
        self.value
            .as_f64()
            .map(|number| number as f32)
            .ok_or_else(|| anyhow!("{} takes a number", self.capability))
    }

    /// A choice's option id. A number is accepted as its text, so `90` and
    /// `"90"` pick the same sweep.
    pub fn option(&self) -> Result<String> {
        match &self.value {
            Value::String(text) => Ok(text.clone()),
            Value::Number(number) => Ok(number.to_string()),
            _ => Err(anyhow!("{} takes one of its options", self.capability)),
        }
    }

    pub fn unknown(&self) -> anyhow::Error {
        anyhow!("no capability {}", self.capability)
    }
}

struct Entry {
    snapshot: Snapshot,
    commands: Option<mpsc::Sender<Set>>,
    /// The device's name, which `rename` changes for every surface at once.
    name: watch::Sender<String>,
}

struct Registry {
    name: String,
    pin: String,
    devices: Vec<Entry>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Registry) -> T) -> Option<T> {
    let mut guard = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    guard.as_mut().map(f)
}

/// Starts the registry for a running bridge.
pub fn begin(config: &Config) {
    *REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(Registry {
        name: config.name.clone(),
        pin: config.homekit_pin.clone(),
        devices: Vec::new(),
    });
}

/// Stops the registry, when this computer stops serving Home.
pub fn end() {
    *REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner()) = None;
}

/// Adds a device, or replaces its snapshot. The name stays the registry's.
pub fn publish(mut snapshot: Snapshot) {
    let readings: Vec<(&str, f64)> = snapshot
        .capabilities
        .iter()
        .filter(|capability| matches!(capability.kind, Kind::Reading { .. }) && capability.active)
        .filter_map(|capability| Some((capability.id, capability.value.as_f64()?)))
        .collect();
    crate::series::observe(&snapshot.id, &readings);
    with(|registry| {
        match registry
            .devices
            .iter_mut()
            .find(|entry| entry.snapshot.id == snapshot.id)
        {
            Some(entry) => {
                snapshot.name = entry.name.borrow().clone();
                entry.snapshot = snapshot;
            }
            None => registry.devices.push(Entry {
                name: watch::Sender::new(snapshot.name.clone()),
                snapshot,
                commands: None,
            }),
        }
    });
}

/// A device's name as it changes, for a surface that shows it, like Home.
pub fn name(id: &str) -> Option<watch::Receiver<String>> {
    with(|registry| {
        let entry = registry
            .devices
            .iter()
            .find(|entry| entry.snapshot.id == id)?;
        Some(entry.name.subscribe())
    })
    .flatten()
}

/// Renames a device everywhere: the config, the menu bar, the CLI and Home.
pub fn rename(device: &str, to: &str) -> Result<()> {
    let to = to.trim();
    with(|registry| {
        let index = find(&registry.devices, device)?;
        let entry = &mut registry.devices[index];
        if entry.snapshot.name == to {
            return Ok(());
        }
        let mut config = config::load()?;
        config::rename(&mut config, &entry.snapshot.id, to)?;
        config::save(&config)?;
        entry.snapshot.name = to.to_owned();
        entry.name.send_replace(to.to_owned());
        Ok(())
    })
    .ok_or_else(|| anyhow!("the bridge is starting"))?
}

/// Where `set` sends a device's requests.
pub fn listen(id: &str, commands: mpsc::Sender<Set>) {
    with(|registry| {
        if let Some(entry) = registry
            .devices
            .iter_mut()
            .find(|entry| entry.snapshot.id == id)
        {
            entry.commands = Some(commands);
        }
    });
}

/// The bridge and its devices, as JSON. None while it is not serving.
pub fn status() -> Option<Value> {
    with(|registry| {
        json!({
            "name": registry.name,
            "pin": registry.pin,
            "paired": config::paired(),
            "devices": registry.devices.iter().map(|entry| &entry.snapshot).collect::<Vec<_>>(),
        })
    })
}

/// One device's snapshot, as JSON.
pub fn get(device: &str) -> Result<Value> {
    snapshot(device).map(|snapshot| json!(snapshot))
}

/// One device's snapshot.
pub fn snapshot(device: &str) -> Result<Snapshot> {
    with(|registry| {
        find(&registry.devices, device).map(|index| registry.devices[index].snapshot.clone())
    })
    .ok_or_else(|| anyhow!("the bridge is starting"))?
}

/// Sends a request to a device and waits for it to act.
pub async fn set(device: &str, capability: &str, value: Value) -> Result<()> {
    let commands = with(|registry| {
        find(&registry.devices, device).map(|index| registry.devices[index].commands.clone())
    })
    .ok_or_else(|| anyhow!("the bridge is starting"))??
    .ok_or_else(|| anyhow!("{device} is not ready yet"))?;
    let (done, answer) = oneshot::channel();
    let request = Set {
        capability: capability.into(),
        value,
        done,
    };
    commands
        .send(request)
        .await
        .map_err(|_| anyhow!("{device} stopped"))?;
    answer.await.map_err(|_| anyhow!("{device} stopped"))?
}

/// A device by its id, name, connector (`dyson`), category (`tv`), or a part
/// of its name that only it has. Case does not matter.
fn find(devices: &[Entry], query: &str) -> Result<usize> {
    let query = query.to_lowercase();
    let exact = |snapshot: &Snapshot| {
        snapshot.id.to_lowercase() == query
            || snapshot.name.to_lowercase() == query
            || snapshot.kind == query
            || snapshot.category == query
    };
    let matching = |test: &dyn Fn(&Snapshot) -> bool| -> Vec<usize> {
        (0..devices.len())
            .filter(|&index| test(&devices[index].snapshot))
            .collect()
    };
    let mut matches = matching(&exact);
    if matches.is_empty() {
        matches = matching(&|snapshot| snapshot.name.to_lowercase().contains(&query));
    }
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(anyhow!("no device matches {query}. Run `hearth devices`")),
        many => Err(anyhow!(
            "{query} matches {}. Be more specific",
            many.iter()
                .map(|&index| devices[index].snapshot.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}
