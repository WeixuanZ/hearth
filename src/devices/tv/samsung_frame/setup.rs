//! Adding a Samsung TV: Allow on the TV, twice. Nothing leaves the network.

use crate::connectors::samsung::{discover, ipc::IpControl, link, protocol::DeviceInfo};
use crate::{
    config,
    devices::tv::samsung_frame::SamsungFrameDevice,
    setup::{given, Answers, Field, Next, Step},
};
use anyhow::{anyhow, Result};
use std::time::Duration;

pub enum Setup {
    Start,
    /// Waiting for Allow on the TV, for the remote.
    Remote {
        host: String,
        name: String,
    },
    /// Waiting for Allow again, for IP Control.
    Ip {
        host: String,
        name: String,
        token: String,
        info: Option<DeviceInfo>,
    },
}

impl Setup {
    pub fn start() -> (Self, Step) {
        let step = Step::new(
            "Get the TV ready",
            "Turn the TV on, out of art mode. On the TV, turn on Settings → General → External \
             Device Manager → Device Connection Manager → Access Notification; Connection → \
             Power On with Mobile; and Network → Expert Settings → IP Remote.",
        )
        .field(Field::text("name", "Name in Home").value("The Frame"))
        .field(
            Field::text("host", "Address")
                .hint("Found on the network if left empty")
                .optional(),
        );
        (Self::Start, step)
    }
}

impl crate::driver::Setup for Setup {
    async fn next(&mut self, answers: &Answers) -> Result<Next> {
        match self {
            Self::Start => {
                let name = given(answers, "name").unwrap_or_else(|| "The Frame".into());
                let host = match given(answers, "host") {
                    Some(host) => host,
                    None => find_one().await?,
                };
                let step = Step::new(
                    "Press Allow on the TV",
                    format!("The TV at {host} asks whether to let hearth control it."),
                )
                .waiting();
                *self = Self::Remote { host, name };
                Ok(Next::Ask(step))
            }
            Self::Remote { host, name } => {
                let token = link::pair(host).await?;
                let info = link::device_info(host).await.ok();
                *self = Self::Ip {
                    host: host.clone(),
                    name: name.clone(),
                    token,
                    info,
                };
                Ok(Next::Ask(
                    Step::new(
                        "Press Allow again",
                        "Now IP Control, which powers the TV fully off and picks inputs. If the TV \
                         is in art mode, leave it first.",
                    )
                    .waiting(),
                ))
            }
            Self::Ip {
                host,
                name,
                token,
                info,
            } => {
                let ip = IpControl::pair(host).await;
                let info = info.take();
                let mut saved = config::load_or_new()?;
                config::upsert(
                    &mut saved,
                    config::Device::new::<crate::devices::tv::samsung_frame::SamsungFrame>(
                        SamsungFrameDevice {
                            name: name.clone(),
                            model: info
                                .as_ref()
                                .map(|info| info.model.clone())
                                .unwrap_or_default(),
                            host: host.clone(),
                            mac: info.map(|info| info.mac).unwrap_or_default(),
                            token: token.clone(),
                            // Its own token is `token`, above.
                            tokens: Default::default(),
                            ip_token: ip.as_ref().cloned().unwrap_or_default(),
                        },
                    ),
                );
                config::save(&saved)?;
                Ok(Next::Done(match ip {
                    Ok(_) => format!("Added {name}."),
                    Err(error) => format!(
                        "Added {name}, without IP Control ({error:#}), so inputs use remote keys. \
                         Turn on IP Remote on the TV and add it again for those."
                    ),
                }))
            }
        }
    }
}

async fn find_one() -> Result<String> {
    let hosts = discover::search(Duration::from_secs(4)).await?;
    match hosts.as_slice() {
        [host] => Ok(host.clone()),
        [] => Err(anyhow!(
            "no Samsung TV answered. Turn it on, or give its address"
        )),
        many => Err(anyhow!(
            "more than one TV answered: {}. Give the address of the one you want",
            many.join(", ")
        )),
    }
}
