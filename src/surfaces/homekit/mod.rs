//! Apple Home: one HomeKit bridge, with each device's accessories behind it.

mod accessory;
pub mod characteristics;
pub mod ids;
mod profile;
mod sync;

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

/// Every device's accessories, made from its capabilities. A device's ids
/// are taken from what hearth gave it before (`ids`), so Home keeps them.
/// Run once the devices have started, before the bridge serves.
pub async fn add_devices(server: &IpServer, config: &Config) -> Result<()> {
    let dir = config::homekit_dir()?;
    let mut ids = ids::Ids::load(&dir)?;
    // Every device's old ids first, so no new id lands on one.
    for device in &config.devices {
        let seed = || {
            crate::driver::find(device.kind())
                .map(|driver| driver.homekit_ids(device))
                .unwrap_or_default()
        };
        ids.seed(&device.id(), seed);
    }
    for device in &config.devices {
        let id = device.id();
        let Ok(snapshot) = crate::capabilities::snapshot(&id) else {
            continue;
        };
        let (writes, asked) = tokio::sync::mpsc::channel(16);
        let made = profile::make(&snapshot, &mut ids, writes).await?;
        sync::add(server, &id, made, asked).await?;
    }
    ids.save(&dir)
}

/// The HomeKit device id, a locally administered MAC derived from `seed`.
fn device_id(seed: &str) -> [u8; 6] {
    let digest = Sha256::digest(seed.as_bytes());
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&digest[..6]);
    mac[0] = (mac[0] | 0x02) & 0xFE;
    mac
}

/// A device's accessories as Home sees them, for tests: the layout Home
/// keys rooms, scenes and automations on, and what its writes ask.
#[cfg(test)]
pub(crate) struct Home {
    made: profile::Made,
    asked: tokio::sync::mpsc::Receiver<accessory::Asks>,
}

#[cfg(test)]
impl Home {
    /// Accessories for `snapshot`, with ids seeded as hearth gave them
    /// before. Fails if any accessory reuses an iid, which Home refuses.
    pub async fn of(snapshot: &crate::capabilities::Snapshot, seed: ids::DeviceIds) -> Self {
        use hap::accessory::HapAccessory;
        let mut ids = ids::Ids::default();
        ids.seed(&snapshot.id, || seed);
        let (writes, asked) = tokio::sync::mpsc::channel(16);
        let made = profile::make(snapshot, &mut ids, writes).await.unwrap();
        for accessory in &made.accessories {
            let mut seen = std::collections::HashSet::new();
            for service in accessory.get_services() {
                let characteristics = service.get_characteristics();
                let iids = std::iter::once(service.get_id()).chain(
                    characteristics
                        .iter()
                        .map(|characteristic| characteristic.get_id()),
                );
                for iid in iids {
                    assert!(
                        seen.insert(iid),
                        "iid {iid} is used twice in accessory {}",
                        accessory.id
                    );
                }
            }
        }
        Self { made, asked }
    }

    /// The iids of the `kind` services on accessory `aid`, in order.
    pub fn services(&self, aid: u64, kind: hap::HapType) -> Vec<u64> {
        use hap::accessory::HapAccessory;
        let Some(accessory) = self
            .made
            .accessories
            .iter()
            .find(|accessory| accessory.id == aid)
        else {
            return Vec::new();
        };
        accessory
            .get_services()
            .into_iter()
            .filter(|service| service.get_type() == kind)
            .map(|service| service.get_id())
            .collect()
    }

    /// Plays Home writing `value` to the `kind` characteristic of service
    /// `iid` on accessory `aid`, and returns what that asks of the device.
    pub async fn write(
        &mut self,
        aid: u64,
        iid: u64,
        kind: hap::HapType,
        value: serde_json::Value,
    ) -> accessory::Asks {
        use hap::accessory::HapAccessory;
        let accessory = self
            .made
            .accessories
            .iter_mut()
            .find(|accessory| accessory.id == aid)
            .unwrap();
        let service = accessory
            .get_mut_services()
            .into_iter()
            .find(|service| service.get_id() == iid)
            .unwrap();
        let characteristic = service.get_mut_characteristic(kind).unwrap();
        characteristic.set_value(value).await.unwrap();
        self.asked.try_recv().unwrap_or_default()
    }

    /// The device as it now is, as the bridge shows it to Home.
    pub fn sees(&self, snapshot: &crate::capabilities::Snapshot) {
        self.made.wiring.seen(std::sync::Arc::new(snapshot.clone()));
    }
}
