//! `~/.config/hearth/config.toml`
//!
//! The bridge (name, pairing code, stable id) is separate from the devices.
//! Each device is one connector. A new connector is a new `kind`.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub name: String,
    pub homekit_pin: String,
    /// Hashed into the HomeKit device id. Changing it makes Home forget the bridge.
    pub bridge_id: String,
    #[serde(default)]
    pub devices: Vec<Device>,
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
    Ok(())
}

/// Forgets every Home that paired with the bridge, so it can be added again.
/// Returns how many pairings there were.
pub fn clear_pairings() -> Result<usize> {
    let dir = homekit_dir()?.join("pairings");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(0);
    };
    let mut removed = 0;
    for entry in entries {
        let path = entry?.path();
        if path.is_file() {
            std::fs::remove_file(&path)
                .with_context(|| format!("could not remove {}", path.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
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
        Err(_) => Ok(Config {
            name: "Hearth".into(),
            homekit_pin: format_pin(&random_pin()),
            bridge_id: String::new(),
            devices: Vec::new(),
        }),
    }
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
                    "Purifier".into()
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

pub fn upsert_dyson(config: &mut Config, device: DysonDevice) {
    if let Some(Device::Dyson(existing)) = config
        .devices
        .iter_mut()
        .find(|item| matches!(item, Device::Dyson(fan) if fan.serial == device.serial))
    {
        let aid = existing.aid;
        *existing = device;
        existing.aid = aid;
        return;
    }
    config.devices.push(Device::Dyson(device));
    assign_aids(&mut config.devices);
}

pub fn upsert_frame(config: &mut Config, device: FrameDevice) {
    if let Some(Device::Frame(existing)) = config
        .devices
        .iter_mut()
        .find(|item| matches!(item, Device::Frame(tv) if tv.host == device.host))
    {
        let aid = existing.aid;
        *existing = device;
        existing.aid = aid;
        return;
    }
    config.devices.push(Device::Frame(device));
    assign_aids(&mut config.devices);
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
        };
        rename(&mut config, "living ROOM", " Television ").unwrap();
        assert_eq!(config.devices[0].name(), "Television");
        assert!(rename(&mut config, "Nothing", "Other").is_err());
        assert!(rename(&mut config, "Television", "TV!").is_err());
        assert!(rename(&mut config, "Television", "  ").is_err());
    }
}
