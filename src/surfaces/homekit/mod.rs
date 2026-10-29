//! Apple Home: one HomeKit bridge, with each device's accessories behind it.

pub mod characteristics;

use crate::config::{self, Config};
use anyhow::{anyhow, Context, Result};
use hap::{
    accessory::{bridge::BridgeAccessory, AccessoryCategory, AccessoryInformation},
    server::{IpServer, Server},
    storage::{FileStorage, Storage},
    MacAddress, Pin,
};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv6Addr};

/// Listen on every address. hap picks the first interface address, often an
/// IPv6 one, while mDNS advertises them all, so Home may dial one we ignore.
/// On macOS `[::]` takes IPv4 too.
const LISTEN_ON: IpAddr = IpAddr::V6(Ipv6Addr::UNSPECIFIED);

/// The bridge, before devices add their accessories.
pub async fn open(config: &Config) -> Result<IpServer> {
    let dir = config::homekit_dir()?;
    std::fs::create_dir_all(&dir)?;
    let mut storage = FileStorage::new(dir.to_str().context("homekit path")?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    let mut hap_config = storage.load_config().await.unwrap_or_default();
    hap_config.pin =
        Pin::new(config::parse_pin(&config.homekit_pin)?).map_err(|error| anyhow!("{error}"))?;
    hap_config.host = LISTEN_ON;
    // Follows bridge_id, so `hearth unpair` can give Home a new accessory.
    hap_config.device_id = MacAddress::new(device_id(&config.bridge_id));
    hap_config.name = config.name.clone();
    hap_config.category = AccessoryCategory::Bridge;
    storage
        .save_config(&hap_config)
        .await
        .map_err(|error| anyhow!("{error}"))?;

    let server = IpServer::new(hap_config, storage)
        .await
        .map_err(|error| anyhow!("HomeKit server failed to start: {error}"))?;
    let bridge = BridgeAccessory::new(
        1,
        AccessoryInformation {
            name: config.name.clone(),
            manufacturer: "Hearth".into(),
            model: "Bridge".into(),
            serial_number: config.bridge_id.clone(),
            firmware_revision: Some(env!("CARGO_PKG_VERSION").into()),
            ..Default::default()
        },
    )
    .map_err(|error| anyhow!("{error}"))?;
    server
        .add_accessory(bridge)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    Ok(server)
}

/// The HomeKit device id, a locally administered MAC derived from `seed`.
fn device_id(seed: &str) -> [u8; 6] {
    let digest = Sha256::digest(seed.as_bytes());
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&digest[..6]);
    mac[0] = (mac[0] | 0x02) & 0xFE;
    mac
}
