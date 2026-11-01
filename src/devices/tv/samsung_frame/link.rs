//! What the Frame's task asks of the outside world: the TV itself, and the
//! config it remembers the TV in. `Network` is the real thing; tests drive the
//! same task against a fake TV.

use crate::{
    config,
    connectors::samsung::{
        ipc::IpControl,
        link,
        protocol::{ArtReply, DeviceInfo},
    },
    devices::tv::samsung_frame::FrameDevice,
};
use anyhow::{anyhow, Result};
use std::{future::Future, time::Duration};

pub trait Link: Send + 'static {
    fn info(&self, device: &FrameDevice) -> impl Future<Output = Result<DeviceInfo>> + Send;
    /// One art-channel request. Empty when the channel stayed silent.
    fn art(
        &self,
        device: &FrameDevice,
        request: &str,
        value: Option<&str>,
    ) -> impl Future<Output = Result<Vec<ArtReply>>> + Send;
    /// Clicks a key, or holds it. Returns a new token if the TV issued one.
    fn press(
        &mut self,
        device: &FrameDevice,
        key: &str,
        hold: Option<Duration>,
    ) -> impl Future<Output = Result<Option<String>>> + Send;
    fn wake(&self, device: &FrameDevice) -> Result<()>;
    /// IP Control, once paired: power and inputs by name rather than keys.
    fn ip_control(&self) -> bool;
    fn set_power(&self, on: bool) -> impl Future<Output = Result<()>> + Send;
    fn input(&self) -> impl Future<Output = Result<String>> + Send;
    fn set_input(&self, source: &str) -> impl Future<Output = Result<()>> + Send;
    /// Keeps what was learned of the TV (token, MAC, model) in the config.
    fn save(&self, device: &FrameDevice);
}

/// The TV over the network.
pub struct Network {
    remote: link::Remote,
    ip: Option<IpControl>,
}

impl Network {
    pub fn new(device: &FrameDevice) -> Self {
        let ip = (!device.ip_token.is_empty())
            .then(|| IpControl::new(&device.host, &device.ip_token).ok())
            .flatten();
        Self {
            remote: link::Remote::default(),
            ip,
        }
    }

    fn ip(&self) -> Result<&IpControl> {
        self.ip
            .as_ref()
            .ok_or_else(|| anyhow!("IP Control is not paired"))
    }
}

impl Link for Network {
    async fn info(&self, device: &FrameDevice) -> Result<DeviceInfo> {
        link::device_info(&device.host).await
    }

    async fn art(
        &self,
        device: &FrameDevice,
        request: &str,
        value: Option<&str>,
    ) -> Result<Vec<ArtReply>> {
        link::art(&device.host, &device.token, request, value).await
    }

    async fn press(
        &mut self,
        device: &FrameDevice,
        key: &str,
        hold: Option<Duration>,
    ) -> Result<Option<String>> {
        self.remote
            .press(&device.host, &device.token, key, hold)
            .await
    }

    fn wake(&self, device: &FrameDevice) -> Result<()> {
        link::wake(&device.mac)
    }

    fn ip_control(&self) -> bool {
        self.ip.is_some()
    }

    async fn set_power(&self, on: bool) -> Result<()> {
        self.ip()?.set_power(on).await
    }

    async fn input(&self) -> Result<String> {
        self.ip()?.input().await
    }

    async fn set_input(&self, source: &str) -> Result<()> {
        self.ip()?.set_input(source).await
    }

    fn save(&self, device: &FrameDevice) {
        let Ok(mut config) = config::load() else {
            return;
        };
        let saved = config.devices.iter_mut().find_map(|item| match item {
            config::Device::Frame(tv) if tv.host == device.host => Some(tv),
            _ => None,
        });
        let Some(saved) = saved else {
            return;
        };
        saved.token = device.token.clone();
        saved.mac = device.mac.clone();
        saved.model = device.model.clone();
        if let Err(error) = config::save(&config) {
            log::warn!("could not save the TV token: {error:#}");
        }
    }
}
