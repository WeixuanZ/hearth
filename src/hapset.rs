//! Small helpers shared by every connector when it talks to HomeKit.

use crate::config;
use anyhow::{anyhow, Result};
use hap::{
    accessory::HapAccessory,
    characteristic::{CharacteristicCallbacks, HapCharacteristic},
    HapType,
};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::mpsc;

/// Shows a Configured Name, as last named in Home or else `default`, and
/// keeps what Home renames it to. Home stores a rename on the accessory, so
/// without this the bridge would show the default again after a restart.
pub async fn configured_name(
    characteristic: &mut (impl HapCharacteristic + CharacteristicCallbacks<String>),
    aid: u64,
    default: &str,
) -> Result<()> {
    let key = format!("{aid}.{}", characteristic.get_id());
    let saved = config::load()
        .ok()
        .and_then(|config| config.names.get(&key).cloned());
    characteristic
        .set_value(serde_json::json!(saved.as_deref().unwrap_or(default)))
        .await
        .map_err(|error| anyhow!("{error}"))?;
    characteristic.on_update(Some(move |_old: &String, new: &String| {
        if let Err(error) = config::save_name(&key, new) {
            log::warn!("could not keep the name {new}: {error:#}");
        }
        Ok(())
    }));
    Ok(())
}

pub fn device_id(seed: &str) -> [u8; 6] {
    let digest = Sha256::digest(seed.as_bytes());
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&digest[..6]);
    mac[0] = (mac[0] | 0x02) & 0xFE;
    mac
}

pub fn hook_u8<T: Send + 'static>(
    characteristic: &mut impl CharacteristicCallbacks<u8>,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<T>,
    make: impl Fn(u8) -> T + Send + Sync + 'static,
) {
    let suppress = suppress.clone();
    let tx = tx.clone();
    characteristic.on_update(Some(move |_old: &u8, new: &u8| {
        if !suppress.load(Ordering::SeqCst) {
            let _ = tx.try_send(make(*new));
        }
        Ok(())
    }));
}

pub fn hook_u32<T: Send + 'static>(
    characteristic: &mut impl CharacteristicCallbacks<u32>,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<T>,
    make: impl Fn(u32) -> T + Send + Sync + 'static,
) {
    let suppress = suppress.clone();
    let tx = tx.clone();
    characteristic.on_update(Some(move |_old: &u32, new: &u32| {
        if !suppress.load(Ordering::SeqCst) {
            let _ = tx.try_send(make(*new));
        }
        Ok(())
    }));
}

pub fn hook_i32<T: Send + 'static>(
    characteristic: &mut impl CharacteristicCallbacks<i32>,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<T>,
    make: impl Fn(i32) -> T + Send + Sync + 'static,
) {
    let suppress = suppress.clone();
    let tx = tx.clone();
    characteristic.on_update(Some(move |_old: &i32, new: &i32| {
        if !suppress.load(Ordering::SeqCst) {
            let _ = tx.try_send(make(*new));
        }
        Ok(())
    }));
}

pub fn hook_bool<T: Send + 'static>(
    characteristic: &mut impl CharacteristicCallbacks<bool>,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<T>,
    make: impl Fn(bool) -> T + Send + Sync + 'static,
) {
    let suppress = suppress.clone();
    let tx = tx.clone();
    characteristic.on_update(Some(move |_old: &bool, new: &bool| {
        if !suppress.load(Ordering::SeqCst) {
            let _ = tx.try_send(make(*new));
        }
        Ok(())
    }));
}

pub fn hook_f32<T: Send + 'static>(
    characteristic: &mut impl CharacteristicCallbacks<f32>,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<T>,
    make: impl Fn(f32) -> T + Send + Sync + 'static,
) {
    let suppress = suppress.clone();
    let tx = tx.clone();
    characteristic.on_update(Some(move |old: &f32, new: &f32| {
        if !suppress.load(Ordering::SeqCst) && (old - new).abs() >= 0.2 {
            let _ = tx.try_send(make(*new));
        }
        Ok(())
    }));
}

pub async fn put(
    accessory: &mut dyn HapAccessory,
    service: HapType,
    kind: HapType,
    value: serde_json::Value,
) -> Result<()> {
    let Some(service) = accessory.get_mut_service(service) else {
        return Ok(());
    };
    if let Some(characteristic) = service.get_mut_characteristic(kind) {
        characteristic
            .set_value(value)
            .await
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}

/// Home refuses a bridge as "out of compliance" when an accessory reuses an
/// iid, so every connector's accessories are checked with this.
#[cfg(test)]
pub fn assert_unique_iids(accessory: &impl serde::Serialize) {
    let value = serde_json::to_value(accessory).unwrap();
    if let Ok(dir) = std::env::var("HEARTH_DUMP") {
        std::fs::write(format!("{dir}/aid{}.json", value["aid"]), value.to_string()).unwrap();
    }
    let mut seen = std::collections::HashSet::new();
    for service in value["services"].as_array().unwrap() {
        let iids = std::iter::once(&service["iid"]).chain(
            service["characteristics"]
                .as_array()
                .unwrap()
                .iter()
                .map(|characteristic| &characteristic["iid"]),
        );
        for iid in iids {
            let iid = iid.as_u64().unwrap();
            assert!(seen.insert(iid), "iid {iid} is used twice in {value}");
        }
    }
}
