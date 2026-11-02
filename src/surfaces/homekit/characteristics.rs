//! Helpers for the accessories hearth shows in Home.

use crate::capabilities;
use anyhow::{anyhow, Result};
use hap::{
    accessory::HapAccessory,
    characteristic::{CharacteristicCallbacks, HapCharacteristic},
    futures::lock::Mutex,
    HapType,
};
use std::sync::Arc;

/// Shows a Configured Name, as last named in Home or else `default`, and
/// keeps what Home renames it to. Home stores a rename on the accessory, so
/// without this the bridge would show the default again after a restart.
pub async fn configured_name(
    characteristic: &mut (impl HapCharacteristic + CharacteristicCallbacks<String>),
    aid: u64,
    default: &str,
) -> Result<()> {
    let key = format!("{aid}.{}", characteristic.get_id());
    let saved = super::names::get(&key);
    characteristic
        .set_value(serde_json::json!(saved.as_deref().unwrap_or(default)))
        .await
        .map_err(|error| anyhow!("{error}"))?;
    characteristic.on_update(Some(move |_old: &String, new: &String| {
        if let Err(error) = super::names::set(&key, new) {
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
    suffix: String,
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
