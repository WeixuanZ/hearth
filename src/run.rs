//! `hearth run`: every device, and the surfaces that show and control them.

use crate::{
    capabilities,
    config::{Config, Device},
    devices,
    surfaces::{control, homekit},
};
use anyhow::{anyhow, Result};
use hap::server::Server;
use tokio::signal::unix::{signal, SignalKind};

pub async fn serve(config: Config) -> Result<()> {
    capabilities::begin(&config);
    let _socket = control::Socket;
    let server = homekit::open(&config).await?;
    for device in config.devices {
        match device {
            Device::Dyson(device) => devices::purifier::spawn(device, server.clone()).await?,
            Device::Frame(device) => devices::tv::spawn(device, server.clone()).await?,
        }
    }
    println!("Apple Home pairing code: {}", config.homekit_pin);
    println!(
        "In the Home app: Add Accessory, More options, then {}.",
        config.name
    );
    println!("This process has to stay awake and on the same network as the devices.");

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = server.run_handle() => result.map_err(|error| anyhow!("HomeKit stopped: {error}"))?,
        result = control::serve() => result?,
        _ = tokio::signal::ctrl_c() => log::info!("stopping"),
        _ = terminate.recv() => log::info!("stopping"),
    }
    Ok(())
}
