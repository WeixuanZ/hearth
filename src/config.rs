//! `~/.config/hearth/config.toml`
//!
//! The bridge (name, pairing code, stable id) is separate from the devices.
//! Each device is one connector. A new connector is a new `kind`.
//!
//! `save` dates whatever changed since the file on disk, so computers that
//! share a bridge can `merge` their copies and keep the newer of each part.

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Device {
    Dyson(DysonDevice),
    Frame(FrameDevice),
    Bosch(BoschDevice),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DysonDevice {
    /// HomeKit accessory id. The fan and its switches are one accessory.
    #[serde(default)]
    pub aid: u64,
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
    /// Returned when you press Allow on the TV. Empty until `hearth frame pair`.
    #[serde(default)]
    pub token: String,
    /// IP Control's token, from a second Allow. Empty if IP Remote is off.
    #[serde(default)]
    pub ip_token: String,
    /// HomeKit accessory id of the art switch, a tile beside the TV.
    #[serde(default)]
    pub art_aid: u64,
}

/// What every kind of device's settings provide. A new kind implements
/// this, adds a variant to [`Device`], and is started in `devices::spawn`.
pub trait Settings {
    /// Stable while the bridge runs: the menu bar, the CLI and Home name it by this.
    fn id(&self) -> String;
    fn name(&self) -> &str;
    fn name_mut(&mut self) -> &mut String;
    /// Every HomeKit accessory id it owns, its own first. 0 until assigned.
    fn aids(&mut self) -> Vec<&mut u64>;
}

impl Settings for DysonDevice {
    fn id(&self) -> String {
        format!("dyson:{}", self.serial)
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn name_mut(&mut self) -> &mut String {
        &mut self.name
    }
    fn aids(&mut self) -> Vec<&mut u64> {
        vec![&mut self.aid]
    }
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
}

/// One value a Bosch feature can take. The key is the number on the wire.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoschFeature {
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, String>,
}

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
}

fn default_mqtt_port() -> u16 {
    1883
}

fn default_true() -> bool {
    true
}

impl Device {
    pub fn settings(&self) -> &dyn Settings {
        match self {
            Self::Dyson(device) => device,
            Self::Frame(device) => device,
            Self::Bosch(device) => device,
        }
    }

    pub fn settings_mut(&mut self) -> &mut dyn Settings {
        match self {
            Self::Dyson(device) => device,
            Self::Frame(device) => device,
            Self::Bosch(device) => device,
        }
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
        "no config yet. Run `hearth dyson login`, `hearth frame pair`, or `hearth bosch login` first ({})",
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
        devices: vec![Device::Dyson(DysonDevice {
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

pub fn upsert_dyson(config: &mut Config, device: DysonDevice) {
    if let Some(Device::Dyson(existing)) = config
        .devices
        .iter_mut()
        .find(|item| matches!(item, Device::Dyson(fan) if fan.serial == device.serial))
    {
        // A fresh login keeps the id and the name chosen since.
        let (aid, name) = (existing.aid, std::mem::take(&mut existing.name));
        *existing = device;
        existing.aid = aid;
        existing.name = name;
        return;
    }
    let mut device = Device::Dyson(device);
    device.set_name(unique_name(config, device.name()));
    config.devices.push(device);
    assign_aids(&mut config.devices);
}

pub fn upsert_frame(config: &mut Config, device: FrameDevice) {
    if let Some(Device::Frame(existing)) = config
        .devices
        .iter_mut()
        .find(|item| matches!(item, Device::Frame(tv) if tv.host == device.host))
    {
        let (aid, art_aid) = (existing.aid, existing.art_aid);
        *existing = device;
        existing.aid = aid;
        existing.art_aid = art_aid;
        return;
    }
    let mut device = Device::Frame(device);
    device.set_name(unique_name(config, device.name()));
    config.devices.push(device);
    assign_aids(&mut config.devices);
}

pub fn upsert_bosch(config: &mut Config, device: BoschDevice) {
    if let Some(Device::Bosch(existing)) = config
        .devices
        .iter_mut()
        .find(|item| matches!(item, Device::Bosch(machine) if machine.ha_id == device.ha_id))
    {
        let (aid, door_aid, leak_aid, cycle_aid, name) = (
            existing.aid,
            existing.door_aid,
            existing.leak_aid,
            existing.cycle_aid,
            std::mem::take(&mut existing.name),
        );
        *existing = device;
        existing.aid = aid;
        existing.door_aid = door_aid;
        existing.leak_aid = leak_aid;
        existing.cycle_aid = cycle_aid;
        existing.name = name;
        return;
    }
    let mut device = Device::Bosch(device);
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
        match &config.devices[0] {
            Device::Dyson(fan) => {
                assert_eq!(fan.aid, 2);
                assert_eq!(fan.serial, "ABC-UK-1");
                assert_eq!(fan.name, "Living room");
                assert!(fan.heating);
            }
            Device::Frame(_) | Device::Bosch(_) => panic!("legacy config is a fan"),
        }
    }

    #[test]
    fn a_new_device_does_not_reuse_the_fan_ids() {
        let mut devices = vec![
            Device::Dyson(DysonDevice {
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
            Device::Frame(FrameDevice {
                name: "The Frame".into(),
                host: "192.168.1.40".into(),
                ..Default::default()
            }),
        ];
        assert!(assign_aids(&mut devices));
        assert_eq!(aid(&devices[1]), 3);
        let Device::Frame(frame) = &devices[1] else {
            unreachable!()
        };
        assert_eq!(frame.art_aid, 4);
        assert!(!assign_aids(&mut devices));
    }

    #[test]
    fn an_art_switch_added_later_keeps_existing_ids() {
        let mut devices = vec![
            Device::Frame(FrameDevice {
                aid: 2,
                host: "10.0.0.2".into(),
                ..Default::default()
            }),
            Device::Dyson(DysonDevice {
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
        let Device::Frame(frame) = &devices[0] else {
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
        upsert_dyson(&mut config, fan("Study fan", "old"));
        upsert_dyson(
            &mut config,
            fan(&crate::connectors::dyson::product_name("XY01"), "new"),
        );
        let Device::Dyson(saved) = &config.devices[0] else {
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
            devices: vec![Device::Frame(FrameDevice {
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
        Device::Dyson(DysonDevice {
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
            upsert_frame(&mut config, tv);
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
    fn a_dishwasher_keeps_its_sensor_ids_and_its_name() {
        let mut devices = vec![
            Device::Dyson(DysonDevice {
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
            Device::Bosch(washer()),
        ];
        assert!(assign_aids(&mut devices));
        let Device::Bosch(machine) = &devices[1] else {
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
        devices.push(Device::Frame(FrameDevice {
            name: "The Frame".into(),
            host: "10.0.0.8".into(),
            ..Default::default()
        }));
        assert!(assign_aids(&mut devices));
        let Device::Frame(frame) = &devices[2] else {
            unreachable!()
        };
        assert_eq!(frame.aid, 7);

        let mut config = Config {
            name: "Hearth".into(),
            homekit_pin: "361-29-487".into(),
            bridge_id: "bridge".into(),
            ..Default::default()
        };
        upsert_bosch(&mut config, washer());
        config.devices[0].set_name("Kitchen".into());
        let text = toml::to_string(&config).unwrap();
        assert!(!text.contains("iv ="));
        let loaded = config_from_toml(&text).unwrap();
        let Device::Bosch(saved) = &loaded.devices[0] else {
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
        upsert_bosch(&mut config, again);
        let Device::Bosch(saved) = &config.devices[0] else {
            unreachable!()
        };
        assert_eq!(saved.name, "Kitchen");
        assert_eq!(saved.key, "rotated");
    }
}
