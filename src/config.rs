use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub name: String,
    pub serial: String,
    pub model: String,
    /// MQTT topic prefix. `auto` tries 527K, then 527E, then 527.
    pub product_type: String,
    pub mqtt_password: String,
    /// LAN address. Empty means discover the fan with mDNS.
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Eight digits, shown as `XXX-XX-XXX` in Apple Home.
    pub homekit_pin: String,
    #[serde(default = "default_true")]
    pub heating: bool,
}

fn default_port() -> u16 {
    1883
}

fn default_true() -> bool {
    true
}

pub fn config_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config").join("dyson-home"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn homekit_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("homekit"))
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    let text = fs::read_to_string(&path).with_context(|| {
        format!(
            "no config yet. Run `dyson-home login` first ({})",
            path.display()
        )
    })?;
    toml::from_str(&text).context("config.toml could not be read")
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
