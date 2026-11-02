//! What the Frame's task asks of the outside world: the TV itself, and the
//! config it remembers the TV in. `Network` is the real thing; tests drive the
//! same task against a fake TV.

use super::protocol::{art_payload, parse_art_message, ArtReply, ART_CHANNEL};
use crate::{
    connectors::samsung::{
        ipc::IpControl,
        link::{self, Channel, Signal},
        protocol::DeviceInfo,
    },
    devices::tv::samsung_frame::SamsungFrameDevice,
    driver::Context,
};
use anyhow::{anyhow, Result};
use std::{future::Future, time::Duration};

pub trait Link: Send + 'static {
    fn info(&self, device: &SamsungFrameDevice) -> impl Future<Output = Result<DeviceInfo>> + Send;
    /// One art-channel request. Empty when the channel stayed silent.
    fn art(
        &self,
        device: &SamsungFrameDevice,
        request: &str,
        value: Option<&str>,
    ) -> impl Future<Output = Result<Vec<ArtReply>>> + Send;
    /// A key, or text, on the remote channel. Returns a new token if the TV
    /// issued one.
    fn remote(
        &mut self,
        device: &SamsungFrameDevice,
        signal: Signal<'_>,
    ) -> impl Future<Output = Result<Option<String>>> + Send;
    fn wake(&self, device: &SamsungFrameDevice) -> Result<()>;
    /// IP Control, once paired: power and inputs by name rather than keys.
    fn ip_control(&self) -> bool;
    fn set_power(&self, on: bool) -> impl Future<Output = Result<()>> + Send;
    fn input(&self) -> impl Future<Output = Result<String>> + Send;
    fn set_input(&self, source: &str) -> impl Future<Output = Result<()>> + Send;
    /// Keeps what was learned of the TV (token, MAC, model) in the config.
    fn save(&self, device: &SamsungFrameDevice);
    /// Shows the TV as it now is, to every surface.
    fn publish(&self, snapshot: crate::capabilities::Snapshot);
}

/// The TV over the network.
pub struct Network {
    remote: link::Remote,
    ip: Option<IpControl>,
    context: Context,
}

impl Network {
    pub fn new(device: &SamsungFrameDevice, context: Context) -> Self {
        let ip = (!device.ip_token.is_empty())
            .then(|| IpControl::new(&device.host, &device.ip_token).ok())
            .flatten();
        Self {
            remote: link::Remote::default(),
            ip,
            context,
        }
    }

    fn ip(&self) -> Result<&IpControl> {
        self.ip
            .as_ref()
            .ok_or_else(|| anyhow!("IP Control is not paired"))
    }
}

impl Link for Network {
    async fn info(&self, device: &SamsungFrameDevice) -> Result<DeviceInfo> {
        link::device_info(&device.host).await
    }

    async fn art(
        &self,
        device: &SamsungFrameDevice,
        request: &str,
        value: Option<&str>,
    ) -> Result<Vec<ArtReply>> {
        art(&device.host, &device.token, request, value).await
    }

    async fn remote(
        &mut self,
        device: &SamsungFrameDevice,
        signal: Signal<'_>,
    ) -> Result<Option<String>> {
        self.remote
            .send_signal(&device.host, &device.token, signal)
            .await
    }

    fn wake(&self, device: &SamsungFrameDevice) -> Result<()> {
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

    fn publish(&self, snapshot: crate::capabilities::Snapshot) {
        self.context.publish(snapshot);
    }

    fn save(&self, device: &SamsungFrameDevice) {
        self.context.remember(|saved: &mut SamsungFrameDevice| {
            saved.token = device.token.clone();
            saved.mac = device.mac.clone();
            saved.model = device.model.clone();
        });
    }
}

/// One request on the art channel, and what it answered within a few
/// seconds. A Frame often stays silent, so no reply is not a failure.
async fn art(host: &str, token: &str, request: &str, value: Option<&str>) -> Result<Vec<ArtReply>> {
    let mut channel = Channel::open(host, ART_CHANNEL, token).await?;
    channel
        .send(art_payload(request, value, &link::request_id()))
        .await?;
    let mut replies = Vec::new();
    let start = tokio::time::Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        let Some(text) = channel.read(Duration::from_secs(3)).await? else {
            break;
        };
        if let Some(reply) = parse_art_message(&text) {
            let answered = matches!(reply, ArtReply::Mode(_));
            replies.push(reply);
            if answered {
                break;
            }
        }
    }
    channel.close().await;
    Ok(replies)
}
