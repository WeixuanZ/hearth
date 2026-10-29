//! `~/.config/hearth/config.toml`
//!
//! The bridge (name, pairing code, stable id) is separate from the devices.
//! Each device is one connector. A new connector is a new `kind`.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// Unix seconds of the last change to the bridge itself (name, code, id).
    #[serde(default)]
    pub updated: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Device {
    Dyson(DysonDevice),
    Frame(FrameDevice),
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
    /// Unix seconds of the last edit. Machines keep the newer record.
    #[serde(default)]
    pub updated: u64,
    /// Set when this machine, or another, removed the device.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
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
    #[serde(default)]
    pub updated: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
}

fn default_mqtt_port() -> u16 {
    1883
}

fn default_true() -> bool {
    true
}

impl Device {
    pub fn aid(&self) -> u64 {
        match self {
            Self::Dyson(device) => device.aid,
            Self::Frame(device) => device.aid,
        }
    }

    pub fn set_aid(&mut self, aid: u64) {
        match self {
            Self::Dyson(device) => device.aid = aid,
            Self::Frame(device) => device.aid = aid,
        }
    }

    /// How many accessory ids this connector consumes, starting at `aid`.
    pub fn aid_span(&self) -> u64 {
        match self {
            Self::Dyson(_) => 1,
            Self::Frame(_) => 1,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Dyson(device) => &device.name,
            Self::Frame(device) => &device.name,
        }
    }

    pub fn set_name(&mut self, name: String) {
        match self {
            Self::Dyson(device) => device.name = name,
            Self::Frame(device) => device.name = name,
        }
    }

    pub fn updated(&self) -> u64 {
        match self {
            Self::Dyson(device) => device.updated,
            Self::Frame(device) => device.updated,
        }
    }

    pub fn removed(&self) -> bool {
        match self {
            Self::Dyson(device) => device.removed,
            Self::Frame(device) => device.removed,
        }
    }

    fn set_updated(&mut self, updated: u64) {
        match self {
            Self::Dyson(device) => device.updated = updated,
            Self::Frame(device) => device.updated = updated,
        }
    }

    fn set_removed(&mut self, removed: bool) {
        match self {
            Self::Dyson(device) => device.removed = removed,
            Self::Frame(device) => device.removed = removed,
        }
    }

    /// Stable id across machines. A Frame prefers its MAC, which does not change when DHCP does.
    pub fn key(&self) -> String {
        match self {
            Self::Dyson(device) => format!("dyson:{}", device.serial.to_ascii_lowercase()),
            Self::Frame(device) => {
                let id = if device.mac.is_empty() {
                    device.host.to_ascii_lowercase()
                } else {
                    device.mac.to_ascii_lowercase()
                };
                format!("frame:{id}")
            }
        }
    }
}

/// Renames the device now called `from`, ignoring case.
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
        .find(|device| device.name().eq_ignore_ascii_case(from.trim()))
        .ok_or_else(|| {
            anyhow::anyhow!("no device is called {from}. Devices: {}", names.join(", "))
        })?;
    device.set_name(to.to_owned());
    device.set_updated(now_secs());
    config.updated = now_secs();
    Ok(())
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
    config.updated = now_secs();
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

pub fn assign_aids(devices: &mut [Device]) -> bool {
    let mut changed = false;
    let mut used = Vec::new();
    for device in devices.iter() {
        if device.aid() >= 2 {
            for offset in 0..device.aid_span() {
                used.push(device.aid() + offset);
            }
        }
        if let Device::Frame(frame) = device {
            if frame.art_aid >= 2 {
                used.push(frame.art_aid);
            }
        }
    }
    let mut next = 2u64;
    for device in devices.iter_mut() {
        if device.aid() >= 2 {
            continue;
        }
        while (0..device.aid_span()).any(|offset| used.contains(&(next + offset))) {
            next += 1;
        }
        device.set_aid(next);
        for offset in 0..device.aid_span() {
            used.push(next + offset);
        }
        next += device.aid_span();
        changed = true;
    }
    // The art switch takes the lowest free id, so ids already in Home stay put.
    for device in devices.iter_mut() {
        if let Device::Frame(frame) = device {
            if frame.art_aid < 2 {
                frame.art_aid = (2..)
                    .find(|aid| !used.contains(aid))
                    .expect("ids are endless");
                used.push(frame.art_aid);
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
        "no config yet. Run `hearth dyson login` or `hearth frame pair` first ({})",
        path.display()
    ))
}

pub fn load_or_new() -> Result<Config> {
    match load() {
        Ok(config) => Ok(config),
        Err(_) => {
            let mut config = Config {
                name: "Hearth".into(),
                homekit_pin: format_pin(&random_pin()),
                bridge_id: String::new(),
                devices: Vec::new(),
                names: BTreeMap::new(),
                updated: 0,
            };
            ensure_bridge_id(&mut config);
            Ok(config)
        }
    }
}

/// A new bridge needs its own HomeKit id. An empty one would make every
/// fresh machine look like the same accessory.
pub fn ensure_bridge_id(config: &mut Config) -> bool {
    if !config.bridge_id.is_empty() {
        return false;
    }
    config.bridge_id = format!("hearth-{:016x}", rand::random::<u64>());
    config.updated = now_secs();
    true
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
                    crate::dyson::product_name(&text("model"))
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
            updated: 0,
            removed: false,
        })],
        names: BTreeMap::new(),
        updated: 0,
    }
}

pub fn save(config: &Config) -> Result<PathBuf> {
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

pub fn upsert_dyson(config: &mut Config, mut device: DysonDevice) {
    device.updated = now_secs();
    device.removed = false;
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
        existing.updated = now_secs();
        config.updated = existing.updated;
        return;
    }
    config.updated = device.updated;
    config.devices.push(Device::Dyson(device));
    assign_aids(&mut config.devices);
}

pub fn upsert_frame(config: &mut Config, mut device: FrameDevice) {
    device.updated = now_secs();
    device.removed = false;
    if let Some(Device::Frame(existing)) = config
        .devices
        .iter_mut()
        .find(|item| matches!(item, Device::Frame(tv) if tv.host == device.host || (!device.mac.is_empty() && tv.mac == device.mac)))
    {
        let (aid, art_aid) = (existing.aid, existing.art_aid);
        let name = std::mem::take(&mut existing.name);
        *existing = device;
        existing.aid = aid;
        existing.art_aid = art_aid;
        if existing.name.is_empty() {
            existing.name = name;
        }
        existing.updated = now_secs();
        config.updated = existing.updated;
        return;
    }
    config.updated = device.updated;
    config.devices.push(Device::Frame(device));
    assign_aids(&mut config.devices);
}

/// Marks a device removed so every machine drops it, and Home loses it on the next restart.
pub fn remove_device(config: &mut Config, name: &str) -> Result<()> {
    let device = config
        .devices
        .iter_mut()
        .find(|device| device.name().eq_ignore_ascii_case(name.trim()) && !device.removed())
        .ok_or_else(|| anyhow!("no device is called {name}"))?;
    let now = now_secs();
    device.set_removed(true);
    device.set_updated(now);
    config.updated = now;
    Ok(())
}

#[derive(Debug, PartialEq)]
pub struct Merged {
    pub config: Config,
    /// The other machine's HomeKit identity should replace this one.
    pub adopt_identity: bool,
    /// Both machines are paired, as different bridges. Nothing was changed.
    pub conflict: bool,
}

/// Combines two machines' device lists. The newer record for each device wins,
/// including a removal. The HomeKit identity follows the machine that is paired,
/// or otherwise the newer edit.
pub fn merge(local: Config, remote: Config, local_paired: bool, remote_paired: bool) -> Merged {
    let ids_differ = !local.bridge_id.is_empty()
        && !remote.bridge_id.is_empty()
        && local.bridge_id != remote.bridge_id;
    if ids_differ && local_paired && remote_paired {
        return Merged {
            config: local,
            adopt_identity: false,
            conflict: true,
        };
    }
    let adopt_identity = if ids_differ {
        if remote_paired {
            true
        } else if local_paired {
            false
        } else {
            // Neither Home has this bridge yet. Agree on one id, or each
            // laptop would advertise its own and Home would see two.
            remote.bridge_id < local.bridge_id
        }
    } else if local_paired && !remote_paired {
        false
    } else {
        (remote.updated > local.updated && !local_paired)
            || (local.bridge_id.is_empty() && !remote.bridge_id.is_empty())
    };

    let mut config = if adopt_identity {
        remote.clone()
    } else {
        local.clone()
    };
    config.devices = merge_devices(&local.devices, &remote.devices);
    config.names = merge_names(&local, &remote, adopt_identity);
    config.updated = local.updated.max(remote.updated);
    if !adopt_identity {
        config.name = local.name;
        config.homekit_pin = local.homekit_pin;
        config.bridge_id = local.bridge_id;
    }
    assign_aids(&mut config.devices);
    Merged {
        config,
        adopt_identity,
        conflict: false,
    }
}

fn merge_devices(local: &[Device], remote: &[Device]) -> Vec<Device> {
    let mut by_key: BTreeMap<String, Device> = BTreeMap::new();
    for device in local {
        by_key.insert(device.key(), device.clone());
    }
    for device in remote {
        match by_key.get(&device.key()) {
            Some(existing) if existing.updated() >= device.updated() => {}
            _ => {
                by_key.insert(device.key(), device.clone());
            }
        }
    }
    by_key.into_values().collect()
}

fn merge_names(local: &Config, remote: &Config, adopt_identity: bool) -> BTreeMap<String, String> {
    let (primary, extra) = if adopt_identity || remote.updated > local.updated {
        (&remote.names, &local.names)
    } else {
        (&local.names, &remote.names)
    };
    let mut names = primary.clone();
    for (key, value) in extra {
        names.entry(key.clone()).or_insert_with(|| value.clone());
    }
    names
}

pub fn revision(config: &Config) -> u64 {
    config.updated.max(
        config
            .devices
            .iter()
            .map(Device::updated)
            .max()
            .unwrap_or(0),
    )
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Devices Home should show. Removals stay in the file so other machines see them.
pub fn live_devices(config: &Config) -> Vec<Device> {
    config
        .devices
        .iter()
        .filter(|device| !device.removed())
        .cloned()
        .collect()
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
            Device::Frame(_) => panic!("legacy config is a fan"),
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
                updated: 0,
                removed: false,
            }),
            Device::Frame(FrameDevice {
                name: "The Frame".into(),
                host: "192.168.1.40".into(),
                ..Default::default()
            }),
        ];
        assert!(assign_aids(&mut devices));
        assert_eq!(devices[1].aid(), 3);
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
                updated: 0,
                removed: false,
            }),
        ];
        assert!(assign_aids(&mut devices));
        assert_eq!((devices[0].aid(), devices[1].aid()), (2, 3));
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
            updated: 0,
            removed: false,
        };
        let mut config = Config {
            name: "Hearth".into(),
            homekit_pin: "000-00-000".into(),
            bridge_id: "ABC".into(),
            devices: Vec::new(),
            names: BTreeMap::new(),
            updated: 0,
        };
        upsert_dyson(&mut config, fan("Study fan", "old"));
        upsert_dyson(&mut config, fan(&crate::dyson::product_name("XY01"), "new"));
        let Device::Dyson(saved) = &config.devices[0] else {
            unreachable!()
        };
        assert_eq!((saved.name.as_str(), saved.aid), ("Study fan", 2));
        assert_eq!(saved.mqtt_password, "new");
        assert_eq!(
            crate::dyson::product_name("HP07"),
            "Dyson Purifier Hot+Cool"
        );
        assert_eq!(crate::dyson::product_name("XY01"), "Dyson Purifier Cool");
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
            names: BTreeMap::new(),
            updated: 0,
        };
        rename(&mut config, "living ROOM", " Television ").unwrap();
        assert_eq!(config.devices[0].name(), "Television");
        assert!(rename(&mut config, "Nothing", "Other").is_err());
        assert!(rename(&mut config, "Television", "TV!").is_err());
        assert!(rename(&mut config, "Television", "  ").is_err());
    }

    fn sample(updated: u64, serial: &str, name: &str) -> Device {
        Device::Dyson(DysonDevice {
            aid: 2,
            name: name.into(),
            serial: serial.into(),
            model: "HP09".into(),
            product_type: "527K".into(),
            mqtt_password: "x".into(),
            host: String::new(),
            port: 1883,
            heating: true,
            updated,
            removed: false,
        })
    }

    #[test]
    fn two_machines_keep_both_devices_and_a_newer_removal() {
        let local = Config {
            name: "Hearth".into(),
            homekit_pin: "111-11-111".into(),
            bridge_id: "same".into(),
            devices: vec![sample(10, "aaa", "Fan")],
            names: BTreeMap::new(),
            updated: 10,
        };
        let remote_fan = sample(20, "bbb", "Other");
        let mut removed = sample(30, "aaa", "Fan");
        if let Device::Dyson(device) = &mut removed {
            device.removed = true;
        }
        let remote = Config {
            name: "Hearth".into(),
            homekit_pin: "111-11-111".into(),
            bridge_id: "same".into(),
            devices: vec![removed, remote_fan],
            names: BTreeMap::new(),
            updated: 30,
        };
        let merged = merge(local, remote, true, false);
        assert!(!merged.conflict);
        assert!(!merged.adopt_identity);
        let live: Vec<_> = merged
            .config
            .devices
            .iter()
            .filter(|device| !device.removed())
            .map(Device::name)
            .collect();
        assert_eq!(live, vec!["Other"]);
    }

    #[test]
    fn an_unpaired_machine_takes_the_paired_identity() {
        let local = Config {
            name: "Hearth".into(),
            homekit_pin: "111-11-111".into(),
            bridge_id: "local".into(),
            devices: vec![sample(5, "aaa", "Fan")],
            names: BTreeMap::new(),
            updated: 5,
        };
        let remote = Config {
            name: "Hearth".into(),
            homekit_pin: "361-29-487".into(),
            bridge_id: "paired".into(),
            devices: vec![sample(4, "bbb", "Other")],
            names: BTreeMap::new(),
            updated: 4,
        };
        let merged = merge(local, remote, false, true);
        assert!(merged.adopt_identity);
        assert_eq!(merged.config.bridge_id, "paired");
        assert_eq!(merged.config.devices.len(), 2);
    }

    #[test]
    fn two_paired_homes_are_left_alone() {
        let local = Config {
            name: "Hearth".into(),
            homekit_pin: "111-11-111".into(),
            bridge_id: "a".into(),
            devices: vec![sample(1, "aaa", "Fan")],
            names: BTreeMap::new(),
            updated: 1,
        };
        let remote = Config {
            name: "Hearth".into(),
            homekit_pin: "222-22-222".into(),
            bridge_id: "b".into(),
            devices: vec![sample(9, "bbb", "Other")],
            names: BTreeMap::new(),
            updated: 9,
        };
        let merged = merge(local, remote, true, true);
        assert!(merged.conflict);
        assert_eq!(merged.config.bridge_id, "a");
        assert_eq!(merged.config.devices.len(), 1);
    }

    #[test]
    fn unpaired_machines_agree_on_one_bridge() {
        let local = Config {
            name: "Hearth".into(),
            homekit_pin: "111-11-111".into(),
            bridge_id: "hearth-bbbb".into(),
            devices: vec![sample(1, "aaa", "Fan")],
            names: BTreeMap::new(),
            updated: 1,
        };
        let remote = Config {
            name: "Hearth".into(),
            homekit_pin: "222-22-222".into(),
            bridge_id: "hearth-aaaa".into(),
            devices: vec![sample(1, "bbb", "Other")],
            names: BTreeMap::new(),
            updated: 1,
        };
        let merged = merge(local.clone(), remote.clone(), false, false);
        assert!(merged.adopt_identity);
        assert_eq!(merged.config.bridge_id, "hearth-aaaa");
        assert_eq!(merged.config.devices.len(), 2);

        let other_way = merge(remote, local, false, false);
        assert!(!other_way.adopt_identity);
        assert_eq!(other_way.config.bridge_id, "hearth-aaaa");
        assert_eq!(other_way.config.devices.len(), 2);
    }
}
