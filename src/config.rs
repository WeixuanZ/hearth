//! `~/.config/hearth/config.toml`
//!
//! The bridge (name, pairing code, stable id) is separate from the devices.
//! Each device is one connector. A new connector is a new `kind`.
//!
//! `save` dates whatever changed since the file on disk, so computers that
//! share a bridge can `merge` their copies and keep the newer of each part.

use crate::{
    devices::purifier::dyson::{Dyson, DysonDevice},
    driver::{self, Driver, Typed},
};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub name: String,
    pub homekit_pin: String,
    /// Hashed into the HomeKit device id. Changing it makes Home forget the bridge.
    pub bridge_id: String,
    #[serde(default)]
    pub devices: Vec<Device>,
    /// Names given in Home, by `aid.iid`. Home keeps a rename on the
    /// accessory, so the bridge saves it here to show it after a restart.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, String>,
    /// When the bridge's own settings above last changed, in Unix seconds.
    #[serde(default)]
    pub updated: u64,
    /// When each device last changed, by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub changed: BTreeMap<String, u64>,
    /// When each removed device went, by id, so other computers drop it too.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub removed: BTreeMap<String, u64>,
    /// Matter's pairing code, while Matter is on (`hearth matter on`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matter: Option<crate::surfaces::matter::Settings>,
}

/// A device in the config: its kind's own settings, read and written by the
/// driver for that kind (see `driver`). A kind this build does not know, as
/// from a newer computer in the group, is kept as it came.
pub struct Device(Box<dyn Entry>);

/// What a [`Device`] holds: one kind's settings, whichever kind.
pub trait Entry: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> &str;
    fn settings(&self) -> &dyn Settings;
    fn settings_mut(&mut self) -> &mut dyn Settings;
    /// Its settings as the config writes them, without `kind` and `id`.
    fn table(&self) -> Result<toml::Table>;
    fn clone_entry(&self) -> Box<dyn Entry>;
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// What every kind of device's settings provide. A new kind implements this
/// for its settings, and is a `driver::Driver`.
pub trait Settings {
    /// Stable while the bridge runs: the menu bar, the CLI and Home name it by this.
    fn id(&self) -> String;
    fn name(&self) -> &str;
    fn name_mut(&mut self) -> &mut String;
    /// Every HomeKit accessory id it owns, its own first. 0 until assigned.
    fn aids(&mut self) -> Vec<&mut u64>;
    /// Where the bridge reaches it on the network, when that is a fixed
    /// address rather than one found each time. `hearth doctor` dials it.
    fn address(&self) -> Option<(String, u16)>;
    /// What setup left to do, if anything.
    fn incomplete(&self) -> Option<&'static str> {
        None
    }
}

impl Device {
    pub fn new<D: Driver>(settings: D::Settings) -> Self {
        Self(Box::new(Typed::<D>(settings)))
    }

    pub fn kind(&self) -> &str {
        self.0.kind()
    }

    /// Its settings, if it is of the kind whose settings are `S`.
    pub fn get<S: 'static>(&self) -> Option<&S> {
        self.0.as_any().downcast_ref()
    }

    pub fn get_mut<S: 'static>(&mut self) -> Option<&mut S> {
        self.0.as_any_mut().downcast_mut()
    }

    pub fn settings(&self) -> &dyn Settings {
        self.0.settings()
    }

    pub fn settings_mut(&mut self) -> &mut dyn Settings {
        self.0.settings_mut()
    }

    pub fn id(&self) -> String {
        self.settings().id()
    }

    pub fn name(&self) -> &str {
        self.settings().name()
    }

    pub fn set_name(&mut self, name: String) {
        *self.settings_mut().name_mut() = name;
    }

    fn written(&self) -> Result<toml::Table> {
        let mut table = toml::Table::new();
        table.insert("kind".into(), self.kind().into());
        table.insert("id".into(), self.id().into());
        table.extend(self.0.table()?);
        Ok(table)
    }
}

impl Clone for Device {
    fn clone(&self) -> Self {
        Self(self.0.clone_entry())
    }
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq for Device {
    fn eq(&self, other: &Self) -> bool {
        matches!((self.written(), other.written()), (Ok(one), Ok(two)) if one == two)
    }
}

impl Serialize for Device {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.written()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Device {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let mut table = toml::Table::deserialize(deserializer)?;
        let mut text = |key: &str| match table.remove(key) {
            Some(toml::Value::String(text)) => Some(text),
            _ => None,
        };
        let kind = text("kind").ok_or_else(|| serde::de::Error::custom("a device with no kind"))?;
        let id = text("id");
        match driver::find(&kind) {
            Some(driver) => driver.read(table).map_err(serde::de::Error::custom),
            None => Ok(Self(Box::new(Unknown::new(kind, id, table)))),
        }
    }
}

/// A device of a kind this build does not know, as it came: never started,
/// but kept, with its name and Home ids, so a newer computer's devices pass
/// through this one unharmed.
#[derive(Debug, Clone)]
struct Unknown {
    kind: String,
    id: String,
    name: String,
    aids: Vec<(String, u64)>,
    table: toml::Table,
}

impl Unknown {
    fn new(kind: String, id: Option<String>, table: toml::Table) -> Self {
        let name = match table.get("name") {
            Some(toml::Value::String(name)) => name.clone(),
            _ => kind.clone(),
        };
        let aids = table
            .iter()
            .filter(|(key, _)| key.ends_with("aid"))
            .filter_map(|(key, value)| {
                Some((key.clone(), u64::try_from(value.as_integer()?).ok()?))
            })
            .collect();
        Self {
            id: id.unwrap_or_else(|| format!("{kind}:{name}")),
            kind,
            name,
            aids,
            table,
        }
    }
}

impl Settings for Unknown {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn name_mut(&mut self) -> &mut String {
        &mut self.name
    }
    fn aids(&mut self) -> Vec<&mut u64> {
        self.aids.iter_mut().map(|(_, aid)| aid).collect()
    }
    fn address(&self) -> Option<(String, u16)> {
        None
    }
}

impl Entry for Unknown {
    fn kind(&self) -> &str {
        &self.kind
    }
    fn settings(&self) -> &dyn Settings {
        self
    }
    fn settings_mut(&mut self) -> &mut dyn Settings {
        self
    }
    fn table(&self) -> Result<toml::Table> {
        let mut table = self.table.clone();
        table.insert("name".into(), self.name.clone().into());
        for (key, aid) in &self.aids {
            table.insert(key.clone(), i64::try_from(*aid)?.into());
        }
        Ok(table)
    }
    fn clone_entry(&self) -> Box<dyn Entry> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Renames the device whose id is `from`, or that is now called `from`,
/// ignoring case.
pub fn rename(config: &mut Config, from: &str, to: &str) -> Result<()> {
    let to = to.trim();
    // Home refuses a name that starts or ends with anything but a letter or digit.
    let edges_ok = to.chars().next().is_some_and(char::is_alphanumeric)
        && to.chars().last().is_some_and(char::is_alphanumeric);
    if !edges_ok {
        anyhow::bail!("a name must start and end with a letter or digit");
    }
    let names: Vec<String> = config
        .devices
        .iter()
        .map(|device| device.name().to_owned())
        .collect();
    let device = config
        .devices
        .iter_mut()
        .find(|device| device.id() == from || device.name().eq_ignore_ascii_case(from.trim()))
        .ok_or_else(|| {
            anyhow::anyhow!("no device is called {from}. Devices: {}", names.join(", "))
        })?;
    device.set_name(to.to_owned());
    Ok(())
}

/// Removes the device whose id is `query`, or that is called `query`.
pub fn remove(config: &mut Config, query: &str) -> Result<String> {
    let index = config
        .devices
        .iter()
        .position(|device| device.id() == query || device.name().eq_ignore_ascii_case(query.trim()))
        .ok_or_else(|| anyhow!("no device is called {query}"))?;
    Ok(config.devices.remove(index).name().to_owned())
}

/// The pairings Home left with the bridge, one file per controller.
fn pairing_files() -> Vec<PathBuf> {
    let Ok(dir) = homekit_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir.join("pairings")) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect()
}

pub fn paired() -> bool {
    !pairing_files().is_empty()
}

/// Keeps a name given in Home, so the next start shows it again.
pub fn save_name(key: &str, name: &str) -> Result<()> {
    let mut config = load()?;
    config.names.insert(key.to_owned(), name.to_owned());
    save(&config)?;
    Ok(())
}

/// Forgets every Home that paired with the bridge, so it can be added again.
/// Returns how many pairings there were.
pub fn clear_pairings() -> Result<usize> {
    let files = pairing_files();
    for path in &files {
        fs::remove_file(path).with_context(|| format!("could not remove {}", path.display()))?;
    }
    Ok(files.len())
}

/// Gives every device's accessories an id, keeping those already in use so
/// Home keeps knowing them. Each device's own accessory comes first.
pub fn assign_aids(devices: &mut [Device]) -> bool {
    let mut slots: Vec<Vec<&mut u64>> = devices
        .iter_mut()
        .map(|device| device.settings_mut().aids())
        .collect();
    // An id claimed twice, as when two computers' devices merge, stays with
    // the first claim; the other gets a new one below.
    let mut used = Vec::new();
    for slot in slots.iter_mut().flatten() {
        if **slot >= 2 && !used.contains(&**slot) {
            used.push(**slot);
        } else {
            **slot = 0;
        }
    }
    let depth = slots.iter().map(Vec::len).max().unwrap_or(0);
    let mut changed = false;
    for level in 0..depth {
        for slot in slots.iter_mut().filter_map(|device| device.get_mut(level)) {
            if **slot == 0 {
                **slot = (2..)
                    .find(|aid| !used.contains(aid))
                    .expect("ids are endless");
                used.push(**slot);
                changed = true;
            }
        }
    }
    changed
}

pub fn config_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config").join("hearth"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn homekit_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("homekit"))
}

fn legacy_config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("dyson-home")
        .join("config.toml"))
}

fn legacy_homekit_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("dyson-home")
        .join("homekit"))
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    if path.exists() {
        let text = fs::read_to_string(&path)?;
        let mut config = config_from_toml(&text)?;
        if assign_aids(&mut config.devices) {
            save(&config)?;
        }
        return Ok(config);
    }
    let legacy = legacy_config_path()?;
    if legacy.exists() {
        let text = fs::read_to_string(&legacy)?;
        let mut config = config_from_toml(&text)?;
        let _ = assign_aids(&mut config.devices);
        let saved = save(&config)?;
        copy_legacy_homekit()?;
        log::info!("moved the fan config to {}", saved.display());
        return Ok(config);
    }
    Err(anyhow!(
        "no config yet. Add a device first, with `hearth add` ({})",
        path.display()
    ))
}

/// The config, or a new one when there is none yet. A config that is there
/// but cannot be read is an error: a new one saved over it would lose every
/// device and the bridge's identity.
pub fn load_or_new() -> Result<Config> {
    if config_path()?.exists() || legacy_config_path()?.exists() {
        return load();
    }
    // Its own identity, or every new bridge would be the same accessory.
    Ok(Config {
        name: "Hearth".into(),
        homekit_pin: format_pin(&random_pin()),
        bridge_id: new_bridge_id(),
        ..Default::default()
    })
}

pub fn new_bridge_id() -> String {
    format!("hearth-{:016x}", rand::random::<u64>())
}

pub fn config_from_toml(text: &str) -> Result<Config> {
    let value: toml::Value = toml::from_str(text).context("config.toml could not be read")?;
    if value.get("devices").is_some() {
        return toml::from_str(text).context("config.toml could not be read");
    }
    if value.get("serial").is_some() {
        return Ok(from_legacy(&value));
    }
    Err(anyhow!(
        "config.toml has neither devices nor a Dyson serial"
    ))
}

fn from_legacy(value: &toml::Value) -> Config {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(|item| item.as_str())
            .unwrap_or("")
            .to_owned()
    };
    let serial = text("serial");
    Config {
        name: "Hearth".into(),
        homekit_pin: {
            let pin = text("homekit_pin");
            if pin.is_empty() {
                format_pin(&random_pin())
            } else {
                pin
            }
        },
        bridge_id: serial.clone(),
        // The first hearth knew only Dyson's purifiers, so a file from then is one.
        devices: vec![Device::new::<Dyson>(DysonDevice {
            aid: 2,
            name: {
                let name = text("name");
                if name.is_empty() {
                    crate::connectors::dyson::product_name(&text("model"))
                } else {
                    name
                }
            },
            serial,
            model: text("model"),
            product_type: text("product_type"),
            mqtt_password: text("mqtt_password"),
            host: text("host"),
            port: value
                .get("port")
                .and_then(|item| item.as_integer())
                .unwrap_or(1883) as u16,
            heating: value
                .get("heating")
                .and_then(|item| item.as_bool())
                .unwrap_or(true),
        })],
        ..Default::default()
    }
}

/// Writes the config, dating what changed since the file on disk.
pub fn save(config: &Config) -> Result<PathBuf> {
    let old = fs::read_to_string(config_path()?)
        .ok()
        .and_then(|text| config_from_toml(&text).ok());
    let mut config = config.clone();
    stamp(&mut config, old.as_ref(), now());
    write(&config)
}

/// Writes the config as it is, dates and all. For a merge.
pub fn write(config: &Config) -> Result<PathBuf> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");
    let text = toml::to_string_pretty(config)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(text.as_bytes())?;
    Ok(path)
}

fn stamp(config: &mut Config, old: Option<&Config>, now: u64) {
    let before = |id: &str| old?.devices.iter().find(|device| device.id() == id);
    for device in &config.devices {
        let id = device.id();
        if before(&id) != Some(device) {
            config.removed.remove(&id);
            config.changed.insert(id, now);
        }
    }
    for device in old.map_or(&[][..], |old| &old.devices) {
        let id = device.id();
        if !config.devices.iter().any(|kept| kept.id() == id) {
            config.changed.remove(&id);
            config.removed.insert(id, now);
        }
    }
    let identity = |config: &Config| {
        (
            config.name.clone(),
            config.homekit_pin.clone(),
            config.bridge_id.clone(),
            config.names.clone(),
        )
    };
    if old.map(identity) != Some(identity(config)) {
        config.updated = now;
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The newest date in the config. A computer with a newer one has news.
pub fn revision(config: &Config) -> u64 {
    config
        .changed
        .values()
        .chain(config.removed.values())
        .copied()
        .fold(config.updated, u64::max)
}

#[derive(Debug, PartialEq)]
pub struct Merged {
    pub config: Config,
    /// Each computer is paired with a Home as a different bridge, so
    /// nothing was merged.
    pub conflict: bool,
}

/// Combines two computers' configs. Each device is the newer of the two
/// copies, or gone if it was removed after that. The bridge's identity
/// follows the computer that is paired with Home; when neither is, both
/// pick the same one, so Home sees one bridge.
pub fn merge(local: &Config, remote: &Config, local_paired: bool, remote_paired: bool) -> Merged {
    let differ = local.bridge_id != remote.bridge_id;
    if differ && local_paired && remote_paired {
        return Merged {
            config: local.clone(),
            conflict: true,
        };
    }
    let theirs = if differ {
        remote_paired || (!local_paired && remote.bridge_id < local.bridge_id)
    } else {
        remote.updated > local.updated
    };
    let mut config = if theirs {
        remote.clone()
    } else {
        local.clone()
    };

    let date = |config: &Config, id: &str| config.changed.get(id).copied().unwrap_or(0);
    let mut devices: BTreeMap<String, (u64, &Device)> = BTreeMap::new();
    for (side, device) in local
        .devices
        .iter()
        .map(|device| (local, device))
        .chain(remote.devices.iter().map(|device| (remote, device)))
    {
        let id = device.id();
        let changed = date(side, &id);
        if devices.get(&id).is_none_or(|(kept, _)| changed > *kept) {
            devices.insert(id, (changed, device));
        }
    }
    config.removed = local.removed.clone();
    for (id, &when) in &remote.removed {
        let kept = config.removed.entry(id.clone()).or_default();
        *kept = (*kept).max(when);
    }
    config.devices.clear();
    config.changed.clear();
    for (id, (changed, device)) in devices {
        if config
            .removed
            .get(&id)
            .is_some_and(|&removed| removed >= changed)
        {
            continue;
        }
        config.removed.remove(&id);
        config.changed.insert(id, changed);
        config.devices.push(device.clone());
    }
    // What both have seen, so neither fetches the other again for it.
    config.updated = local.updated.max(remote.updated);
    assign_aids(&mut config.devices);
    Merged {
        config,
        conflict: false,
    }
}

fn copy_legacy_homekit() -> Result<()> {
    let from = legacy_homekit_dir()?;
    let to = homekit_dir()?;
    if to.exists() || !from.exists() {
        return Ok(());
    }
    copy_dir(&from, &to)
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Adds `device`, or puts it in place of the one with its id, keeping the
/// name given since and Home's accessory ids, so Home sees the same device.
pub fn upsert(config: &mut Config, mut device: Device) {
    let id = device.id();
    if let Some(existing) = config.devices.iter_mut().find(|item| item.id() == id) {
        let aids: Vec<u64> = existing
            .settings_mut()
            .aids()
            .into_iter()
            .map(|aid| *aid)
            .collect();
        for (slot, aid) in device.settings_mut().aids().into_iter().zip(aids) {
            *slot = aid;
        }
        device.set_name(existing.name().to_owned());
        *existing = device;
        return;
    }
    device.set_name(unique_name(config, device.name()));
    config.devices.push(device);
    assign_aids(&mut config.devices);
}

/// `name`, or `name 2`, `name 3`… if a device already has it, so a second TV
/// of the same model can be told apart.
fn unique_name(config: &Config, name: &str) -> String {
    let taken = |candidate: &str| {
        config
            .devices
            .iter()
            .any(|device| device.name().eq_ignore_ascii_case(candidate))
    };
    (1..)
        .map(|n| {
            if n == 1 {
                name.to_owned()
            } else {
                format!("{name} {n}")
            }
        })
        .find(|candidate| !taken(candidate))
        .expect("names are endless")
}

pub fn format_pin(digits: &[u8; 8]) -> String {
    format!(
        "{}{}{}-{}{}-{}{}{}",
        digits[0], digits[1], digits[2], digits[3], digits[4], digits[5], digits[6], digits[7]
    )
}

pub fn parse_pin(text: &str) -> Result<[u8; 8]> {
    let digits: Vec<u8> = text
        .chars()
        .filter(|char| char.is_ascii_digit())
        .map(|char| char as u8 - b'0')
        .collect();
    if digits.len() != 8 {
        anyhow::bail!("HomeKit PIN must be 8 digits");
    }
    let mut pin = [0u8; 8];
    pin.copy_from_slice(&digits);
    Ok(pin)
}

pub fn random_pin() -> [u8; 8] {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    loop {
        let mut pin = [0u8; 8];
        for digit in &mut pin {
            *digit = rng.gen_range(0..10);
        }
        if pin.iter().all(|digit| *digit == pin[0]) {
            continue;
        }
        if pin == [1, 2, 3, 4, 5, 6, 7, 8] || pin == [8, 7, 6, 5, 4, 3, 2, 1] {
            continue;
        }
        return pin;
    }
}

trait OpenOptionsExt {
    fn mode(&mut self, mode: u32) -> &mut Self;
}

impl OpenOptionsExt for fs::OpenOptions {
    fn mode(&mut self, mode: u32) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::bosch::protocol::BoschFeature;
    use crate::devices::{
        dishwasher::bosch::{Bosch, BoschDevice},
        tv::samsung_frame::{Frame, FrameDevice},
    };

    fn aid(device: &Device) -> u64 {
        *device.clone().settings_mut().aids()[0]
    }

    #[test]
    fn legacy_fan_keeps_its_accessory_ids() {
        let text = r#"
            name = "Living room"
            serial = "ABC-UK-1"
            model = "HP09"
            product_type = "527K"
            mqtt_password = "secret"
            host = "192.168.1.20"
            port = 1883
            homekit_pin = "361-29-487"
            heating = true
        "#;
        let config = config_from_toml(text).unwrap();
        assert_eq!(config.bridge_id, "ABC-UK-1");
        assert_eq!(config.homekit_pin, "361-29-487");
        let fan = config.devices[0]
            .get::<DysonDevice>()
            .expect("legacy config is a fan");
        assert_eq!(fan.aid, 2);
        assert_eq!(fan.serial, "ABC-UK-1");
        assert_eq!(fan.name, "Living room");
        assert!(fan.heating);
    }

    #[test]
    fn a_new_device_does_not_reuse_the_fan_ids() {
        let mut devices = vec![
            Device::new::<Dyson>(DysonDevice {
                aid: 2,
                name: "Purifier".into(),
                serial: "ABC".into(),
                model: "HP09".into(),
                product_type: "527K".into(),
                mqtt_password: "x".into(),
                host: String::new(),
                port: 1883,
                heating: true,
            }),
            Device::new::<Frame>(FrameDevice {
                name: "The Frame".into(),
                host: "192.168.1.40".into(),
                ..Default::default()
            }),
        ];
        assert!(assign_aids(&mut devices));
        assert_eq!(aid(&devices[1]), 3);
        let Some(frame) = devices[1].get::<FrameDevice>() else {
            unreachable!()
        };
        assert_eq!(frame.art_aid, 4);
        assert!(!assign_aids(&mut devices));
    }

    #[test]
    fn an_art_switch_added_later_keeps_existing_ids() {
        let mut devices = vec![
            Device::new::<Frame>(FrameDevice {
                aid: 2,
                host: "10.0.0.2".into(),
                ..Default::default()
            }),
            Device::new::<Dyson>(DysonDevice {
                aid: 3,
                name: "Purifier".into(),
                serial: "ABC".into(),
                model: "XY01".into(),
                product_type: "000X".into(),
                mqtt_password: "x".into(),
                host: String::new(),
                port: 1883,
                heating: true,
            }),
        ];
        assert!(assign_aids(&mut devices));
        assert_eq!((aid(&devices[0]), aid(&devices[1])), (2, 3));
        let Some(frame) = devices[0].get::<FrameDevice>() else {
            unreachable!()
        };
        assert_eq!(frame.art_aid, 4);
    }

    #[test]
    fn a_fresh_login_keeps_the_name_and_id() {
        let fan = |name: &str, password: &str| DysonDevice {
            aid: 0,
            name: name.into(),
            serial: "ABC".into(),
            model: "XY01".into(),
            product_type: "000X".into(),
            mqtt_password: password.into(),
            host: String::new(),
            port: 1883,
            heating: true,
        };
        let mut config = Config {
            name: "Hearth".into(),
            homekit_pin: "000-00-000".into(),
            bridge_id: "ABC".into(),
            ..Default::default()
        };
        upsert(&mut config, Device::new::<Dyson>(fan("Study fan", "old")));
        upsert(
            &mut config,
            Device::new::<Dyson>(fan(&crate::connectors::dyson::product_name("XY01"), "new")),
        );
        let Some(saved) = config.devices[0].get::<DysonDevice>() else {
            unreachable!()
        };
        assert_eq!((saved.name.as_str(), saved.aid), ("Study fan", 2));
        assert_eq!(saved.mqtt_password, "new");
        assert_eq!(
            crate::connectors::dyson::product_name("HP07"),
            "Dyson Purifier Hot+Cool"
        );
        assert_eq!(
            crate::connectors::dyson::product_name("XY01"),
            "Dyson Purifier Cool"
        );
    }

    #[test]
    fn rename_finds_a_device_by_name_and_checks_the_new_one() {
        let mut config = Config {
            name: "Hearth".into(),
            homekit_pin: "000-00-000".into(),
            bridge_id: "ABC".into(),
            devices: vec![Device::new::<Frame>(FrameDevice {
                aid: 2,
                name: "Living room".into(),
                host: "10.0.0.2".into(),
                ..Default::default()
            })],
            ..Default::default()
        };
        rename(&mut config, "living ROOM", " Television ").unwrap();
        assert_eq!(config.devices[0].name(), "Television");
        assert!(rename(&mut config, "Nothing", "Other").is_err());
        assert!(rename(&mut config, "Television", "TV!").is_err());
        assert!(rename(&mut config, "Television", "  ").is_err());
    }

    fn fan(serial: &str, name: &str) -> Device {
        Device::new::<Dyson>(DysonDevice {
            aid: 2,
            name: name.into(),
            serial: serial.into(),
            model: "XY01".into(),
            product_type: "000X".into(),
            mqtt_password: "x".into(),
            host: String::new(),
            port: 1883,
            heating: true,
        })
    }

    fn bridge(id: &str, devices: Vec<Device>) -> Config {
        Config {
            name: "Hearth".into(),
            homekit_pin: "111-22-333".into(),
            bridge_id: id.into(),
            devices,
            ..Default::default()
        }
    }

    #[test]
    fn a_second_device_of_a_model_gets_its_own_name() {
        let mut config = bridge("a", Vec::new());
        for host in ["10.0.0.2", "10.0.0.3"] {
            let tv = FrameDevice {
                name: "The Frame".into(),
                host: host.into(),
                ..Default::default()
            };
            upsert(&mut config, Device::new::<Frame>(tv));
        }
        let names: Vec<_> = config.devices.iter().map(Device::name).collect();
        assert_eq!(names, ["The Frame", "The Frame 2"]);
        assert_ne!(aid(&config.devices[0]), aid(&config.devices[1]));
    }

    #[test]
    fn saving_dates_what_changed() {
        let old = bridge("a", vec![fan("AAA", "Fan"), fan("BBB", "Other")]);
        let mut new = old.clone();
        new.devices = vec![fan("AAA", "Study fan")];
        stamp(&mut new, Some(&old), 50);
        assert_eq!(new.changed.get("dyson:AAA"), Some(&50));
        assert_eq!(new.removed.get("dyson:BBB"), Some(&50));
        assert_eq!(new.updated, 0);

        let mut same = new.clone();
        stamp(&mut same, Some(&new), 60);
        assert_eq!(same, new);
        same.name = "Home".into();
        stamp(&mut same, Some(&new), 70);
        assert_eq!(same.updated, 70);
    }

    #[test]
    fn a_merge_keeps_the_newer_device_and_a_later_removal() {
        let mut local = bridge("a", vec![fan("AAA", "Fan"), fan("CCC", "Kept")]);
        local.changed = BTreeMap::from([("dyson:AAA".into(), 10), ("dyson:CCC".into(), 40)]);
        let mut remote = bridge("a", vec![fan("BBB", "Other")]);
        remote.changed = BTreeMap::from([("dyson:BBB".into(), 20)]);
        remote.removed = BTreeMap::from([("dyson:AAA".into(), 30), ("dyson:CCC".into(), 30)]);

        let merged = merge(&local, &remote, true, false);
        assert!(!merged.conflict);
        let names: Vec<_> = merged.config.devices.iter().map(Device::name).collect();
        assert_eq!(names, ["Other", "Kept"]);
        assert_eq!(
            merged.config.removed.keys().collect::<Vec<_>>(),
            ["dyson:AAA"]
        );
        // Both fans came with aid 2; the second gets another.
        let aids: Vec<_> = merged.config.devices.iter().map(aid).collect();
        assert_eq!(aids, [2, 3]);
        // Merging again changes nothing.
        assert_eq!(
            merge(&merged.config, &remote, true, false).config,
            merged.config
        );
    }

    #[test]
    fn the_identity_follows_home_or_else_the_lower_id() {
        let local = bridge("hearth-b", vec![fan("AAA", "Fan")]);
        let remote = bridge("hearth-a", vec![fan("BBB", "Other")]);
        let both = |local_paired, remote_paired| {
            merge(&local, &remote, local_paired, remote_paired)
                .config
                .bridge_id
        };
        assert_eq!(both(false, false), "hearth-a");
        assert_eq!(both(true, false), "hearth-b");
        assert_eq!(both(false, true), "hearth-a");
        assert_eq!(
            merge(&remote, &local, false, false).config.bridge_id,
            "hearth-a"
        );

        let homes = merge(&local, &remote, true, true);
        assert!(homes.conflict);
        assert_eq!(homes.config, local);
    }

    #[test]
    fn a_dishwashers_bare_name_is_dialled_on_local() {
        let mut device = washer();
        let at = |device: &BoschDevice| {
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

    fn washer() -> BoschDevice {
        BoschDevice {
            aid: 0,
            door_aid: 0,
            leak_aid: 0,
            cycle_aid: 0,
            name: "Bosch Dishwasher".into(),
            brand: "Bosch".into(),
            model: "SPV2HKX42G".into(),
            ha_id: "BOSCH-HA".into(),
            host: "BOSCH-Dishwasher-BOSCH-HA".into(),
            key: "secret".into(),
            iv: String::new(),
            features: BTreeMap::from([(
                "200".into(),
                BoschFeature {
                    name: "BSH.Common.Setting.PowerState".into(),
                    values: BTreeMap::from([("0".into(), "Off".into()), ("1".into(), "On".into())]),
                },
            )]),
        }
    }

    #[test]
    fn a_kind_this_build_does_not_know_passes_through_unharmed() {
        let text = r#"
name = "Hearth"
homekit_pin = "111-22-333"
bridge_id = "abc"

[[devices]]
kind = "lamp"
id = "lamp:1"
name = "Desk lamp"
aid = 9
bulb = "warm"
"#;
        let mut config = config_from_toml(text).unwrap();
        let lamp = &config.devices[0];
        assert_eq!(
            (lamp.kind(), lamp.id(), lamp.name()),
            ("lamp", "lamp:1".into(), "Desk lamp")
        );
        // Its Home id is held, so a new device does not take it.
        assert!(!assign_aids(&mut config.devices));
        config.devices[0].set_name("Reading lamp".into());
        let written = toml::to_string(&config).unwrap();
        let again = config_from_toml(&written).unwrap();
        assert_eq!(again.devices[0].name(), "Reading lamp");
        assert!(written.contains("bulb = \"warm\"") && written.contains("aid = 9"));
    }

    #[test]
    fn a_dishwasher_keeps_its_sensor_ids_and_its_name() {
        let mut devices = vec![
            Device::new::<Dyson>(DysonDevice {
                aid: 2,
                name: "Purifier".into(),
                serial: "ABC".into(),
                model: "HP09".into(),
                product_type: "527K".into(),
                mqtt_password: "x".into(),
                host: String::new(),
                port: 1883,
                heating: true,
            }),
            Device::new::<Bosch>(washer()),
        ];
        assert!(assign_aids(&mut devices));
        let Some(machine) = devices[1].get::<BoschDevice>() else {
            unreachable!()
        };
        assert_eq!(
            (
                machine.aid,
                machine.door_aid,
                machine.leak_aid,
                machine.cycle_aid
            ),
            (3, 4, 5, 6)
        );
        devices.push(Device::new::<Frame>(FrameDevice {
            name: "The Frame".into(),
            host: "10.0.0.8".into(),
            ..Default::default()
        }));
        assert!(assign_aids(&mut devices));
        let Some(frame) = devices[2].get::<FrameDevice>() else {
            unreachable!()
        };
        assert_eq!(frame.aid, 7);

        let mut config = Config {
            name: "Hearth".into(),
            homekit_pin: "361-29-487".into(),
            bridge_id: "bridge".into(),
            ..Default::default()
        };
        upsert(&mut config, Device::new::<Bosch>(washer()));
        config.devices[0].set_name("Kitchen".into());
        let text = toml::to_string(&config).unwrap();
        assert!(!text.contains("iv ="));
        let loaded = config_from_toml(&text).unwrap();
        let Some(saved) = loaded.devices[0].get::<BoschDevice>() else {
            unreachable!()
        };
        assert_eq!(saved.name, "Kitchen");
        assert_eq!(saved.key, "secret");
        assert!(saved.iv.is_empty());
        assert_eq!(
            saved.features["200"].values.get("1").map(String::as_str),
            Some("On")
        );
        let mut again = washer();
        again.key = "rotated".into();
        again.name = "Bosch Dishwasher".into();
        let mut config = loaded;
        upsert(&mut config, Device::new::<Bosch>(again));
        let Some(saved) = config.devices[0].get::<BoschDevice>() else {
            unreachable!()
        };
        assert_eq!(saved.name, "Kitchen");
        assert_eq!(saved.key, "rotated");
    }
}
