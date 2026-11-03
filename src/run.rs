//! `hearth run`: the control socket and the group always, and on the
//! computer that serves, every device, the HomeKit bridge and Matter.

use crate::{
    capabilities, cluster,
    config::{Config, Device},
    devices, series,
    surfaces::{control, homekit},
};
use anyhow::{anyhow, Result};
use hap::server::Server;
use tokio::{
    runtime::Runtime,
    signal::unix::{signal, SignalKind},
    task::JoinHandle,
};

pub async fn run() -> Result<()> {
    let _socket = control::Socket;
    series::start();
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = control::serve() => result?,
        result = cluster::run() => result?,
        _ = tokio::signal::ctrl_c() => log::info!("stopping"),
        _ = terminate.recv() => log::info!("stopping"),
    }
    series::shutdown();
    Ok(())
}

/// What serving is made from: a change to any of it serves again, as a
/// device's new key or Matter turned on. A device's name is not part of it:
/// a rename reaches every surface while serving.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    name: String,
    pin: String,
    bridge: String,
    matter: Option<crate::surfaces::matter::Settings>,
    devices: Vec<Device>,
}

impl Layout {
    pub fn of(config: &Config) -> Self {
        let devices = config
            .devices
            .iter()
            .map(|device| {
                let mut device = device.clone();
                device.set_name(String::new());
                device
            })
            .collect();
        Self {
            name: config.name.clone(),
            pin: config.homekit_pin.clone(),
            bridge: config.bridge_id.clone(),
            matter: config.matter,
            devices,
        }
    }
}

/// The devices, Home and Matter, served until dropped. They run on a
/// runtime of their own, so stopping ends every connection they opened.
pub struct Serving {
    runtime: Option<Runtime>,
    task: JoinHandle<()>,
    /// What it serves, to tell when that changes.
    pub layout: Layout,
}

impl Serving {
    pub fn start(config: Config) -> Result<Self> {
        let layout = Layout::of(&config);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("hearth-serve")
            .enable_all()
            .build()?;
        let task = runtime.spawn(async move {
            if let Err(error) = serve(config).await {
                log::error!("{error:#}");
            }
        });
        Ok(Self {
            runtime: Some(runtime),
            task,
            layout,
        })
    }

    pub fn stopped(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        capabilities::end();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

async fn serve(config: Config) -> Result<()> {
    capabilities::begin(&config);
    let server = homekit::open(&config).await?;
    for device in config.devices.clone() {
        devices::spawn(device).await?;
    }
    // Home's accessories, made from what the devices just published.
    homekit::add_devices(&server, &config).await?;
    // Matter too, if it is on, until serving ends.
    let _matter = crate::surfaces::matter::start(&config)?;
    log::info!(
        "serving Home as {}. To add it: Add Accessory, More options, then {}, code {}",
        config.name,
        config.name,
        config.homekit_pin
    );
    server
        .run_handle()
        .await
        .map_err(|error| anyhow!("HomeKit stopped: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::test::{Lamp, LampSettings};

    fn config(secret: &str, name: &str) -> Config {
        Config {
            name: "Hearth".into(),
            homekit_pin: "111-22-333".into(),
            bridge_id: "a".into(),
            devices: vec![Device::new::<Lamp>(LampSettings {
                secret: secret.into(),
                ..LampSettings::new("A", name)
            })],
            ..Default::default()
        }
    }

    #[test]
    fn serving_starts_again_for_new_settings_or_matter_but_not_a_rename() {
        let now = Layout::of(&config("old", "Lamp"));
        assert_eq!(Layout::of(&config("old", "Reading lamp")), now);
        assert_ne!(Layout::of(&config("new", "Lamp")), now);
        let mut matter = config("old", "Lamp");
        matter.matter = Some(crate::surfaces::matter::Settings::new());
        assert_ne!(Layout::of(&matter), now);
    }
}
