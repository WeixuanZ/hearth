//! `~/.config/hearth/config.toml`
//!
//! The bridge (name, pairing code, stable id) is separate from the devices.
//! Each device is one connector. A new connector is a new `kind`.
//!
//! `save` dates whatever changed since the file on disk, so computers that
//! share a bridge can `merge` their copies and keep the newer of each part.

use crate::driver::{self, Driver, Typed};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub name: String,
    pub homekit_pin: String,
    /// Hashed into the HomeKit device id. Changing it makes Home forget the bridge.
    pub bridge_id: String,
    #[serde(default)]
    pub devices: Vec<Device>,
    /// Names given in Home, as a config from before `homekit/names.json`
    /// kept them: read once, to move them there, and not written again.
    #[serde(default, skip_serializing)]
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
/// but kept, with its name and everything else it had, so a newer
/// computer's devices pass through this one unharmed.
#[derive(Debug, Clone)]
struct Unknown {
    kind: String,
    id: String,
    name: String,
    table: toml::Table,
}

impl Unknown {
    fn new(kind: String, id: Option<String>, table: toml::Table) -> Self {
        let name = match table.get("name") {
            Some(toml::Value::String(name)) => name.clone(),
            _ => kind.clone(),
        };
        Self {
            id: id.unwrap_or_else(|| format!("{kind}:{name}")),
            kind,
            name,
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

/// Forgets every Home that paired with the bridge, so it can be added again.
/// Returns how many pairings there were.
pub fn clear_pairings() -> Result<usize> {
    let files = pairing_files();
    for path in &files {
        fs::remove_file(path).with_context(|| format!("could not remove {}", path.display()))?;
    }
    Ok(files.len())
}

pub fn config_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config").join("hearth"))
}

/// This computer's id among the others in a group, kept across restarts.
pub fn computer_id() -> Result<String> {
    let path = config_dir()?.join("node");
    if let Some(id) = fs::read_to_string(&path)
        .ok()
        .filter(|id| !id.trim().is_empty())
    {
        return Ok(id.trim().to_owned());
    }
    let id = format!("{:016x}", rand::random::<u64>());
    crate::files::write(&path, id.as_bytes())?;
    Ok(id)
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
        return config_from_toml(&text);
    }
    let legacy = legacy_config_path()?;
    if legacy.exists() {
        let text = fs::read_to_string(&legacy)?;
        let config = config_from_toml(&text)?;
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
    Err(anyhow!("config.toml lists no devices"))
}

/// Writes the config, dating what changed since the file on disk.
pub fn save(config: &Config) -> Result<PathBuf> {
    let old = fs::read_to_string(config_path()?)
        .ok()
        .and_then(|text| config_from_toml(&text).ok());
    let mut config = config.clone();
    stamp(&mut config, old.as_ref(), crate::clock::now());
    write(&config)
}

/// Writes the config as it is, dates and all. For a merge.
pub fn write(config: &Config) -> Result<PathBuf> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");
    crate::files::write(&path, toml::to_string_pretty(config)?.as_bytes())?;
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
        )
    };
    if old.map(identity) != Some(identity(config)) {
        config.updated = now;
    }
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
/// name given since. Its id is what every surface knows it by, so they all
/// see the same device.
pub fn upsert(config: &mut Config, mut device: Device) {
    let id = device.id();
    if let Some(existing) = config.devices.iter_mut().find(|item| item.id() == id) {
        device.set_name(existing.name().to_owned());
        *existing = device;
        return;
    }
    device.set_name(unique_name(config, device.name()));
    config.devices.push(device);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::test::{Lamp, LampSettings};

    fn lamp(key: &str, name: &str) -> Device {
        Device::new::<Lamp>(LampSettings::new(key, name))
    }

    #[test]
    fn adding_one_again_keeps_its_name_and_id_and_takes_what_it_learned() {
        let mut config = bridge("a", Vec::new());
        let named = |name: &str, secret: &str| {
            Device::new::<Lamp>(LampSettings {
                secret: secret.into(),
                ..LampSettings::new("A", name)
            })
        };
        upsert(&mut config, named("Study lamp", "old"));
        upsert(&mut config, named("Lamp", "new"));
        let Some(saved) = config.devices[0].get::<LampSettings>() else {
            unreachable!()
        };
        assert_eq!(saved.name, "Study lamp");
        assert_eq!(saved.secret, "new");
    }

    #[test]
    fn rename_finds_a_device_by_name_and_checks_the_new_one() {
        let mut config = bridge("a", vec![lamp("A", "Living room")]);
        rename(&mut config, "living ROOM", " Reading lamp ").unwrap();
        assert_eq!(config.devices[0].name(), "Reading lamp");
        assert!(rename(&mut config, "Nothing", "Other").is_err());
        assert!(rename(&mut config, "Reading lamp", "Lamp!").is_err());
        assert!(rename(&mut config, "Reading lamp", "  ").is_err());
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
        for key in ["A", "B"] {
            upsert(
                &mut config,
                Device::new::<Lamp>(LampSettings::new(key, "Lamp")),
            );
        }
        let names: Vec<_> = config.devices.iter().map(Device::name).collect();
        assert_eq!(names, ["Lamp", "Lamp 2"]);
    }

    #[test]
    fn saving_dates_what_changed() {
        let old = bridge("a", vec![lamp("AAA", "Lamp"), lamp("BBB", "Other")]);
        let mut new = old.clone();
        new.devices = vec![lamp("AAA", "Study lamp")];
        stamp(&mut new, Some(&old), 50);
        assert_eq!(new.changed.get("test-lamp:AAA"), Some(&50));
        assert_eq!(new.removed.get("test-lamp:BBB"), Some(&50));
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
        let mut local = bridge("a", vec![lamp("AAA", "Lamp"), lamp("CCC", "Kept")]);
        local.changed =
            BTreeMap::from([("test-lamp:AAA".into(), 10), ("test-lamp:CCC".into(), 40)]);
        let mut remote = bridge("a", vec![lamp("BBB", "Other")]);
        remote.changed = BTreeMap::from([("test-lamp:BBB".into(), 20)]);
        remote.removed =
            BTreeMap::from([("test-lamp:AAA".into(), 30), ("test-lamp:CCC".into(), 30)]);

        let merged = merge(&local, &remote, true, false);
        assert!(!merged.conflict);
        let names: Vec<_> = merged.config.devices.iter().map(Device::name).collect();
        assert_eq!(names, ["Other", "Kept"]);
        assert_eq!(
            merged.config.removed.keys().collect::<Vec<_>>(),
            ["test-lamp:AAA"]
        );
        // Merging again changes nothing.
        assert_eq!(
            merge(&merged.config, &remote, true, false).config,
            merged.config
        );
    }

    #[test]
    fn the_identity_follows_home_or_else_the_lower_id() {
        let local = bridge("hearth-b", vec![lamp("AAA", "Lamp")]);
        let remote = bridge("hearth-a", vec![lamp("BBB", "Other")]);
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
    fn a_kind_known_by_an_older_name_is_read_and_written_by_its_new_one() {
        let text = r#"
name = "Hearth"
homekit_pin = "111-22-333"
bridge_id = "abc"

[[devices]]
kind = "test-light"
id = "test-lamp:A"
key = "A"
name = "Desk lamp"
"#;
        let config = config_from_toml(text).unwrap();
        assert_eq!(config.devices[0].kind(), "test-lamp");
        let written = toml::to_string(&config).unwrap();
        assert!(written.contains("kind = \"test-lamp\""), "{written}");
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
        config.devices[0].set_name("Reading lamp".into());
        let written = toml::to_string(&config).unwrap();
        let again = config_from_toml(&written).unwrap();
        assert_eq!(again.devices[0].name(), "Reading lamp");
        assert!(written.contains("bulb = \"warm\"") && written.contains("aid = 9"));
    }
}
