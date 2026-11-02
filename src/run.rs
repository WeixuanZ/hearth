//! `hearth run`: the control socket and the group always, and on the
//! computer that serves Home, every device and the HomeKit bridge.

use crate::{
    capabilities, cluster,
    config::Config,
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

/// Home and the devices, served until dropped. They run on a runtime of
/// their own, so stopping ends every connection they opened.
pub struct Serving {
    runtime: Option<Runtime>,
    task: JoinHandle<()>,
    /// What needs a restart to show in Home. See `cluster::layout`.
    pub layout: String,
}

impl Serving {
    pub fn start(config: Config, layout: String) -> Result<Self> {
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
