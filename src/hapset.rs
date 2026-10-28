//! Small helpers shared by every connector when it talks to HomeKit.

use anyhow::{anyhow, Result};
use hap::{accessory::HapAccessory, characteristic::CharacteristicCallbacks, HapType};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::mpsc;

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
