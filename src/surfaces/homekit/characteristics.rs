//! Helpers every device uses to build its HomeKit accessories.

use crate::{capabilities, config};
use anyhow::{anyhow, Result};
use hap::{
    accessory::HapAccessory,
    characteristic::{CharacteristicCallbacks, HapCharacteristic},
    futures::lock::Mutex,
    HapType,
};
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

/// Shows the device's name as the Configured Name of its main service, the
/// name Home follows. A rename in Home renames the device everywhere.
pub async fn device_name(
    characteristic: &mut (impl HapCharacteristic + CharacteristicCallbacks<String>),
    id: String,
    name: &str,
) -> Result<()> {
    characteristic
        .set_value(serde_json::json!(name))
        .await
        .map_err(|error| anyhow!("{error}"))?;
    characteristic.on_update(Some(move |_old: &String, new: &String| {
        if let Err(error) = capabilities::rename(&id, new) {
            log::warn!("could not rename {id} to {new}: {error:#}");
        }
        Ok(())
    }));
    Ok(())
}

/// Brings a rename from the menu bar or the CLI to Home, on the Configured
/// Name that `device_name` set up on `service`.
pub fn follow_name(id: &str, accessory: Arc<Mutex<Box<dyn HapAccessory>>>, service: HapType) {
    follow(id, accessory, service, str::to_owned);
}

/// As `follow_name`, for an accessory of the device's own, named after it,
/// like "{name} door".
pub fn follow_label(
    id: &str,
    accessory: Arc<Mutex<Box<dyn HapAccessory>>>,
    service: HapType,
    suffix: &'static str,
) {
    follow(id, accessory, service, move |name| {
        format!("{name} {suffix}")
    });
}

fn follow(
    id: &str,
    accessory: Arc<Mutex<Box<dyn HapAccessory>>>,
    service: HapType,
    label: impl Fn(&str) -> String + Send + 'static,
) {
    let Some(mut names) = capabilities::name(id) else {
        return;
    };
    tokio::spawn(async move {
        while names.changed().await.is_ok() {
            let name = label(&names.borrow_and_update());
            let mut accessory = accessory.lock().await;
            let characteristic = accessory
                .get_mut_service(service)
                .and_then(|service| service.get_mut_characteristic(HapType::ConfiguredName));
            if let Some(characteristic) = characteristic {
                if let Err(error) = characteristic.set_value(serde_json::json!(name)).await {
                    log::warn!("could not show {name} in Home: {error}");
                }
            }
        }
    });
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
