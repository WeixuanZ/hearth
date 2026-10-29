//! Samsung The Frame, 2024 (LS03D, including the 65").
//!
//! Nothing here talks to SmartThings. The TV is on the LAN: a secure websocket
//! on port 8002 for keys and art mode, IP Control on port 1516 for power and
//! inputs, and a wake packet when it is fully off. Turn on Settings → General →
//! External Device Manager → Device Connection Manager → Access Notification,
//! Connection → Power On with Mobile, and Network → Expert Settings → IP Remote.

mod accessory;
mod discover;
mod ipc;
mod link;
mod protocol;

use crate::config::{self, FrameDevice};
use accessory::{apply, apply_art, build, build_art, View};
use anyhow::{anyhow, Result};
use hap::server::{IpServer, Server};
use ipc::IpControl;
use protocol::{ArtReply, Input};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone)]
pub enum Command {
    Active(bool),
    Art(bool),
    Input(u32),
    Key(u8),
    Brightness(i32),
    /// Home's volume button. HAP says 0 is louder, but Home on iOS sends 0 for
    /// the down button, as tested on a Frame.
    Volume(u8),
    Mute,
}

pub async fn pair(host: Option<String>, name: String) -> Result<()> {
    let host = match host {
        Some(host) => host,
        None => find_one().await?,
    };
    println!("Turn {host} on. If the screen asks, press Allow.");
    let token = link::pair(&host).await?;
    let info = link::device_info(&host).await.ok();
    if let Some(info) = &info {
        if !info.model.is_empty() {
            println!("The TV says it is {}.", info.model);
        }
        if !info.frame {
            println!("It did not describe itself as a Frame. Art mode may do nothing.");
        }
    }
    println!("Now IP Control, for power and inputs. Leave art mode, then press Allow again.");
    let ip_token = match IpControl::pair(&host).await {
        Ok(token) => token,
        Err(error) => {
            println!("Skipped IP Control ({error:#}). Inputs fall back to remote keys.");
            println!("Turn on Settings → General → Network → Expert Settings → IP Remote,");
            println!("then run this again.");
            String::new()
        }
    };
    let mac = info
        .as_ref()
        .map(|info| info.mac.clone())
        .unwrap_or_default();
    let mut saved = config::load_or_new()?;
    if saved.bridge_id.is_empty() {
        saved.bridge_id = if mac.is_empty() {
            host.clone()
        } else {
            mac.clone()
        };
    }
    config::upsert_frame(
        &mut saved,
        FrameDevice {
            name,
            model: info.map(|info| info.model).unwrap_or_default(),
            host,
            mac,
            token,
            ip_token,
            ..Default::default()
        },
    );
    let path = config::save(&saved)?;
    println!("Saved {}.", path.display());
    println!("Apple Home pairing code: {}", saved.homekit_pin);
    println!("Next: hearth run");
    Ok(())
}

async fn find_one() -> Result<String> {
    println!("Looking for a Samsung TV on the network.");
    let hosts = discover::search(Duration::from_secs(4)).await?;
    match hosts.as_slice() {
        [host] => Ok(host.clone()),
        [] => Err(anyhow!(
            "no Samsung TV answered. Turn it on and pass --host"
        )),
        many => {
            println!("More than one TV answered:");
            for host in many {
                println!("  {host}");
            }
            Err(anyhow!("pass --host with the one you want"))
        }
    }
}

pub async fn status(device: &FrameDevice) -> Result<()> {
    let mut tv = Tv::new(device.clone());
    tv.refresh().await?;
    println!("{}  {}", device.name, tv.summary());
    Ok(())
}

/// Home's id for `tv` or an HDMI number, as typed on the command line.
pub fn input_id(name: &str) -> u32 {
    let source = match name {
        "tv" => "TV".to_owned(),
        hdmi => format!("HDMI{hdmi}"),
    };
    Input::from_source(&source).map_or(0, |input| input as u32)
}

fn configured() -> Result<FrameDevice> {
    config::load()?
        .devices
        .into_iter()
        .find_map(|device| match device {
            config::Device::Frame(device) => Some(device),
            _ => None,
        })
        .ok_or_else(|| anyhow!("no Frame in the config. Run `hearth frame pair` first"))
}

pub async fn keys(keys: Vec<String>, hold: Option<f32>) -> Result<()> {
    let mut tv = Tv::new(configured()?);
    let hold = hold.map(Duration::from_secs_f32);
    for key in keys {
        tv.press(&key, hold).await?;
        println!("sent {key}");
    }
    // Give the TV a moment to read the last key before the socket closes.
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(())
}

/// Runs `command` the way Home does, then prints what the TV reports.
/// Without a command, only prints.
pub async fn control(command: Option<Command>) -> Result<()> {
    let mut tv = Tv::new(configured()?);
    // A TV in deep standby does not answer until it is woken.
    let _ = tv.refresh().await;
    if let Some(command) = command {
        tv.handle(command).await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        tv.refresh().await?;
    }
    println!("{}", tv.summary());
    Ok(())
}

pub async fn spawn(device: FrameDevice, server: IpServer) -> Result<()> {
    let (tx, rx) = mpsc::channel(32);
    let tv = Tv::new(device);
    let (state_tx, mut state_rx) = watch::channel(tv.view());
    let suppress = Arc::new(AtomicBool::new(false));
    let accessory = server
        .add_accessory(build(&tv.device, &suppress, &tx).await?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    let art = server
        .add_accessory(build_art(&tv.device, &suppress, &tx).await?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    tokio::spawn(async move {
        let updates = async move {
            loop {
                let current = state_rx.borrow().clone();
                suppress.store(true, Ordering::SeqCst);
                let result = async {
                    apply(accessory.lock().await.as_mut(), &current).await?;
                    apply_art(art.lock().await.as_mut(), &current).await
                }
                .await;
                suppress.store(false, Ordering::SeqCst);
                if let Err(error) = result {
                    log::warn!("Frame HomeKit update failed: {error:#}");
                }
                if state_rx.changed().await.is_err() {
                    break;
                }
            }
        };
        tokio::pin!(updates);
        tokio::select! {
            result = tv.run(rx, state_tx) => {
                if let Err(error) = result {
                    log::error!("Frame stopped: {error:#}");
                }
            }
            _ = &mut updates => {}
        }
    });
    Ok(())
}

/// The TV as hearth last saw it, and the connections it keeps open.
struct Tv {
    device: FrameDevice,
    remote: link::Remote,
    /// Set once IP Control is paired. Without it, power and inputs use keys.
    ip: Option<IpControl>,
    on: bool,
    input: u32,
    brightness: Option<i32>,
    art: Option<bool>,
    reachable: bool,
    seen: bool,
}

impl Tv {
    fn new(device: FrameDevice) -> Self {
        let ip = (!device.ip_token.is_empty())
            .then(|| IpControl::new(&device.host, &device.ip_token).ok())
            .flatten();
        Self {
            device,
            remote: link::Remote::default(),
            ip,
            on: false,
            input: 0,
            brightness: None,
            art: None,
            reachable: false,
            seen: false,
        }
    }

    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        states: watch::Sender<View>,
    ) -> Result<()> {
        let mut poll = tokio::time::interval(Duration::from_secs(20));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    if let Err(error) = self.handle(command).await {
                        log::warn!("{}: {error:#}", self.device.name);
                    }
                }
                _ = poll.tick() => {
                    if let Err(error) = self.refresh().await {
                        log::debug!("{}: {error:#}", self.device.name);
                        self.reachable = false;
                        self.seen = true;
                        self.on = false;
                    }
                }
            }
            states.send(self.view()).ok();
            crate::status::update(self.snap());
        }
        Ok(())
    }

    fn view(&self) -> View {
        View {
            on: self.on,
            art: self.on && self.art == Some(true),
            input: self.input,
            brightness: self.brightness,
        }
    }

    fn summary(&self) -> String {
        let input = Input::from_id(self.input).map_or("-", Input::label);
        let art = self
            .art
            .map_or("unknown", |on| if on { "on" } else { "off" });
        let power = if self.on { "on" } else { "standby" };
        format!("power {power}  art {art}  input {input}")
    }

    fn snap(&self) -> crate::status::DeviceSnap {
        let headline = if !self.seen {
            "Looking"
        } else if !self.reachable {
            "Unreachable"
        } else if !self.on {
            "Standby"
        } else if self.art == Some(true) {
            "Art"
        } else {
            Input::from_id(self.input).map_or("On", Input::label)
        };
        let mut metrics = Vec::new();
        if self.seen && self.reachable {
            metrics.push(crate::status::Metric {
                label: "Power".into(),
                value: if self.on { "On" } else { "Standby" }.into(),
            });
        }
        if let Some(level) = self.brightness {
            metrics.push(crate::status::Metric {
                label: "Art light".into(),
                value: level.to_string(),
            });
        }
        crate::status::DeviceSnap {
            id: format!("frame:{}", self.device.host),
            kind: "frame".into(),
            name: self.device.name.clone(),
            online: self.reachable,
            headline: headline.into(),
            detail: self.device.model.clone(),
            metrics,
            seen: if self.seen {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0)
            } else {
                0
            },
        }
    }

    async fn refresh(&mut self) -> Result<()> {
        let info = link::device_info(&self.device.host).await?;
        self.on = info.on;
        self.reachable = true;
        self.seen = true;
        if self.device.mac.is_empty() && !info.mac.is_empty()
            || self.device.model.is_empty() && !info.model.is_empty()
        {
            self.device.mac = info.mac;
            self.device.model = info.model;
            self.remember();
        }
        if !self.on {
            // Out of standby it may come back in art or showing a source.
            self.art = None;
            return Ok(());
        }
        if let Some(ip) = &self.ip {
            match ip.input().await {
                Ok(source) => match Input::from_source(&source) {
                    Some(input) => self.input = input as u32,
                    None => log::debug!("unknown input {source}"),
                },
                // It refuses in art mode, which keeps the last input.
                Err(error) => log::debug!("input: {error:#}"),
            }
        }
        if self.device.token.is_empty() {
            return Ok(());
        }
        // The art channel opens a few seconds after the picture. Until then
        // the TV is on and art mode is simply not known yet.
        let replies = link::art(
            &self.device.host,
            &self.device.token,
            "get_artmode_status",
            None,
        )
        .await
        .unwrap_or_else(|error| {
            log::debug!("art: {error:#}");
            Vec::new()
        });
        for reply in replies {
            match reply {
                ArtReply::Mode(on) => self.art = Some(on),
                ArtReply::Brightness(level) => {
                    self.brightness = Some(protocol::home_brightness(level));
                }
            }
        }
        Ok(())
    }

    async fn handle(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Active(true) => self.power_on().await?,
            Command::Active(false) => self.power_off().await?,
            Command::Input(id) => {
                let Some(input) = Input::from_id(id) else {
                    return Ok(());
                };
                if !self.on {
                    self.power_on().await?;
                }
                if self.art != Some(false) {
                    let _ = self.set_art(false).await;
                }
                match &self.ip {
                    Some(ip) => ip.set_input(input.source()).await?,
                    None => self.click(&input.key()).await?,
                }
                self.input = id;
            }
            Command::Art(on) => {
                if on && !self.on {
                    self.power_on().await?;
                }
                if self.on {
                    self.set_art(on).await?;
                }
            }
            Command::Key(value) => {
                if let Some(key) = protocol::hap_remote_key(value) {
                    self.click(key).await?;
                }
            }
            Command::Brightness(home) => {
                let level = protocol::art_brightness(home);
                self.brightness = Some(protocol::home_brightness(level));
                let level = level.to_string();
                let replies = link::art(
                    &self.device.host,
                    &self.device.token,
                    "set_brightness",
                    Some(&level),
                )
                .await?;
                if replies.is_empty() {
                    log::info!("the TV did not confirm the art brightness");
                }
            }
            Command::Volume(0) => self.click("KEY_VOLDOWN").await?,
            Command::Volume(_) => self.click("KEY_VOLUP").await?,
            Command::Mute => self.click("KEY_MUTE").await?,
        }
        Ok(())
    }

    /// From standby, the TV comes back showing a picture, not art.
    async fn power_on(&mut self) -> Result<()> {
        let by_ip = match &self.ip {
            Some(ip) => ip.set_power(true).await.is_ok(),
            None => false,
        };
        if !by_ip {
            match link::device_info(&self.device.host).await {
                Ok(info) if info.on => {
                    self.on = true;
                    return Ok(());
                }
                // In network standby the remote channel still listens.
                Ok(_) => self.click("KEY_POWER").await?,
                // Fully asleep, it answers only a wake packet, which may be enough.
                Err(_) => {
                    link::wake(&self.device.mac)?;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let woke = link::device_info(&self.device.host)
                        .await
                        .is_ok_and(|info| info.on);
                    if !woke {
                        self.click("KEY_POWER").await?;
                    }
                }
            }
        }
        self.on = true;
        self.art = Some(false);
        Ok(())
    }

    async fn power_off(&mut self) -> Result<()> {
        let by_ip = match &self.ip {
            Some(ip) => ip.set_power(false).await.is_ok(),
            None => false,
        };
        if !by_ip {
            // A click toggles art, and the 2024 firmware ignores KEY_POWEROFF.
            // Only a held power key, repeated as a remote does, turns it off.
            self.press("KEY_POWER", Some(Duration::from_secs(4)))
                .await?;
        }
        self.on = false;
        self.art = None;
        Ok(())
    }

    async fn set_art(&mut self, on: bool) -> Result<()> {
        let value = if on { "on" } else { "off" };
        let replies = link::art(
            &self.device.host,
            &self.device.token,
            "set_artmode_status",
            Some(value),
        )
        .await
        .unwrap_or_default();
        if !replies.is_empty() {
            self.art = Some(on);
            return Ok(());
        }
        // The art channel can stay silent. One power click moves a Frame
        // showing a source into art, and one click in art leaves it.
        if self.art != Some(on) {
            self.click("KEY_POWER").await?;
            self.art = Some(on);
        }
        Ok(())
    }

    async fn click(&mut self, key: &str) -> Result<()> {
        self.press(key, None).await
    }

    async fn press(&mut self, key: &str, hold: Option<Duration>) -> Result<()> {
        if self.device.token.is_empty() {
            anyhow::bail!("not paired. Run `hearth frame pair` while the TV is on");
        }
        let token = self
            .remote
            .press(&self.device.host, &self.device.token, key, hold)
            .await?;
        if let Some(token) = token.filter(|token| *token != self.device.token) {
            self.device.token = token;
            self.remember();
        }
        Ok(())
    }

    fn remember(&self) {
        let Ok(mut config) = config::load() else {
            return;
        };
        let saved = config.devices.iter_mut().find_map(|item| match item {
            config::Device::Frame(tv) if tv.host == self.device.host => Some(tv),
            _ => None,
        });
        let Some(saved) = saved else {
            return;
        };
        saved.token = self.device.token.clone();
        saved.mac = self.device.mac.clone();
        saved.model = self.device.model.clone();
        if let Err(error) = config::save(&config) {
            log::warn!("could not save the TV token: {error:#}");
        }
    }
}
