//! One HomeKit bridge. Connectors add their accessories, then this serves them.

use crate::{
    config::{self, Config},
    dyson, frame,
    hapset::device_id,
};
use anyhow::{anyhow, Context, Result};
use hap::{
    accessory::{bridge::BridgeAccessory, AccessoryCategory, AccessoryInformation},
    server::{IpServer, Server},
    storage::{FileStorage, Storage},
    Config as HapConfig, MacAddress, Pin,
};

pub async fn serve(config: Config) -> Result<()> {
    let server = open(&config).await?;
    for device in config.devices {
        match device {
            config::Device::Dyson(device) => dyson::spawn(device, server.clone()).await?,
            config::Device::Frame(device) => frame::spawn(device, server.clone()).await?,
        }
    }
    println!("Apple Home pairing code: {}", config.homekit_pin);
    println!(
        "In the Home app: Add Accessory, More options, then {}.",
        config.name
    );
    println!("This process has to stay awake and on the same network as the devices.");

    tokio::select! {
        result = server.run_handle() => {
            result.map_err(|error| anyhow!("HomeKit stopped: {error}"))?;
        }
        _ = tokio::signal::ctrl_c() => log::info!("stopping"),
    }
    Ok(())
}

async fn open(config: &Config) -> Result<IpServer> {
    let dir = config::homekit_dir()?;
    std::fs::create_dir_all(&dir)?;
    let mut storage = FileStorage::new(dir.to_str().context("homekit path")?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    let pin =
        Pin::new(config::parse_pin(&config.homekit_pin)?).map_err(|error| anyhow!("{error}"))?;
    let hap_config = match storage.load_config().await {
        Ok(mut existing) => {
            existing.redetermine_local_ip();
            existing.name = config.name.clone();
            existing.category = AccessoryCategory::Bridge;
            storage
                .save_config(&existing)
                .await
                .map_err(|error| anyhow!("{error}"))?;
            existing
        }
        Err(_) => {
            let created = HapConfig {
                pin,
                name: config.name.clone(),
                device_id: MacAddress::new(device_id(&config.bridge_id)),
                category: AccessoryCategory::Bridge,
                ..Default::default()
            };
            storage
                .save_config(&created)
                .await
                .map_err(|error| anyhow!("{error}"))?;
            created
        }
    };
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
