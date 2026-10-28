//! A snapshot the optional macOS menu bar reads.
//!
//! `hearth run` rewrites `~/.config/hearth/status.json` while it is alive.
//! The file is mode 0600. It has names and readings, not passwords.

use crate::config::{self, Config, Device};
use anyhow::Result;
use serde::Serialize;
use std::{
    fs,
    io::Write,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Metric {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeviceSnap {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub online: bool,
    pub headline: String,
    pub detail: String,
    pub metrics: Vec<Metric>,
    /// Unix seconds of the last reading. Zero means none yet.
    pub seen: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct Snapshot {
    pid: u32,
    updated: u64,
    name: String,
    pin: String,
    devices: Vec<DeviceSnap>,
}

static STATE: Mutex<Option<Snapshot>> = Mutex::new(None);

pub fn begin(config: &Config) {
    let devices = config.devices.iter().map(waiting).collect::<Vec<_>>();
    let snap = Snapshot {
        pid: std::process::id(),
        updated: now(),
        name: config.name.clone(),
        pin: config.homekit_pin.clone(),
        devices,
    };
    let mut guard = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    *guard = Some(snap);
    let state = guard.clone();
    drop(guard);
    if let Some(state) = state {
        let _ = write(&state);
    }
}

pub fn update(device: DeviceSnap) {
    let mut guard = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(state) = guard.as_mut() else {
        return;
    };
    if let Some(existing) = state.devices.iter_mut().find(|item| item.id == device.id) {
        *existing = device;
    } else {
        state.devices.push(device);
    }
    state.updated = now();
    let copy = state.clone();
    drop(guard);
    let _ = write(&copy);
}

pub fn flush() {
    let mut guard = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(state) = guard.as_mut() else {
        return;
    };
    state.pid = std::process::id();
    state.updated = now();
    let copy = state.clone();
    drop(guard);
    let _ = write(&copy);
}

pub fn finish() {
    let mut guard = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    *guard = None;
    drop(guard);
    if let Ok(path) = status_path() {
        let _ = fs::remove_file(path);
    }
}

fn waiting(device: &Device) -> DeviceSnap {
    match device {
        Device::Dyson(device) => DeviceSnap {
            id: format!("dyson:{}", device.serial),
            kind: "dyson".into(),
            name: device.name.clone(),
            online: false,
            headline: "Looking".into(),
            detail: device.model.clone(),
            metrics: Vec::new(),
            seen: 0,
        },
        Device::Frame(device) => DeviceSnap {
            id: format!("frame:{}", device.host),
            kind: "frame".into(),
            name: device.name.clone(),
            online: false,
            headline: "Looking".into(),
            detail: device.model.clone(),
            metrics: Vec::new(),
            seen: 0,
        },
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn status_path() -> Result<std::path::PathBuf> {
    Ok(config::config_dir()?.join("status.json"))
}

fn write(snapshot: &Snapshot) -> Result<()> {
    let dir = config::config_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join("status.json");
    let tmp = dir.join("status.json.tmp");
    let text = serde_json::to_string_pretty(snapshot)?;
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
    }
    fs::rename(&tmp, &path)?;
    Ok(())
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
    fn a_waiting_fan_is_named_and_has_no_secret() {
        let device = DeviceSnap {
            id: "dyson:ABC".into(),
            kind: "dyson".into(),
            name: "Purifier".into(),
            online: false,
            headline: "Looking".into(),
            detail: "HP09".into(),
            metrics: vec![Metric {
                label: "PM2.5".into(),
                value: "8".into(),
            }],
            seen: 0,
        };
        let text = serde_json::to_string(&device).unwrap();
        assert!(text.contains("Purifier"));
        assert!(!text.contains("mqtt"));
    }
}
