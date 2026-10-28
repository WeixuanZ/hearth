//! Samsung The Frame, 2024 (LS03D, including the 65").
//!
//! Nothing here talks to SmartThings. The TV is on the LAN: a secure websocket
//! on port 8002 for keys and art mode, and a wake packet when it is fully off.
//! Turn on Settings → General → External Device Manager → Device Connection
//! Manager → Access Notification, and Connection → Power On with Mobile.

mod accessory;
mod discover;
mod link;
mod protocol;

use crate::config::{self, FrameDevice};
use accessory::{apply, build, View};
use anyhow::{anyhow, Result};
use hap::server::{IpServer, Server};
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
    Input(u32),
    Key(u8),
    Brightness(i32),
    /// 0 is louder, 1 is quieter. That is how Home sends a relative volume button.
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
    let mut saved = config::load_or_new()?;
    if saved.bridge_id.is_empty() {
        saved.bridge_id = info
            .as_ref()
            .map(|info| info.mac.clone())
            .filter(|mac| !mac.is_empty())
            .unwrap_or_else(|| host.clone());
    }
    config::upsert_frame(
        &mut saved,
        FrameDevice {
            aid: 0,
            name,
            model: info
                .as_ref()
                .map(|info| info.model.clone())
                .unwrap_or_default(),
            host: host.clone(),
            mac: info
                .as_ref()
                .map(|info| info.mac.clone())
                .unwrap_or_default(),
            token,
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
    let info = link::device_info(&device.host).await?;
    println!(
        "{}  {}",
        device.name,
        if info.on { "on" } else { "standby" }
    );
    if !info.model.is_empty() {
        println!("model {}", info.model);
    }
    println!(
        "token {}",
        if device.token.is_empty() {
            "not paired"
        } else {
            "saved"
        }
    );
    if info.on && !device.token.is_empty() {
        match link::art(&device.host, &device.token, "get_artmode_status", None).await {
            Ok(replies) => match replies
                .into_iter()
                .find(|reply| matches!(reply, ArtReply::Mode(_)))
            {
                Some(ArtReply::Mode(on)) => println!("art {}", if on { "on" } else { "off" }),
                _ => println!("art channel did not say"),
            },
            Err(error) => println!("art: {error:#}"),
        }
    }
    Ok(())
}

pub async fn spawn(device: FrameDevice, server: IpServer) -> Result<()> {
    let (tx, rx) = mpsc::channel(32);
    let (state_tx, mut state_rx) = watch::channel(View {
        on: false,
        input: 0,
        brightness: None,
    });
    let suppress = Arc::new(AtomicBool::new(false));
    let accessory = server
        .add_accessory(build(&device, &suppress, &tx).await?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    tokio::spawn(async move {
        let updates = async move {
            loop {
                let current = state_rx.borrow().clone();
                suppress.store(true, Ordering::SeqCst);
                if let Err(error) = apply(accessory.lock().await.as_mut(), &current).await {
                    log::warn!("Frame HomeKit update failed: {error:#}");
                }
                suppress.store(false, Ordering::SeqCst);
                if state_rx.changed().await.is_err() {
                    break;
                }
            }
        };
        tokio::pin!(updates);
        tokio::select! {
            result = run(device, rx, state_tx) => {
                if let Err(error) = result {
                    log::error!("Frame stopped: {error:#}");
                }
            }
            _ = &mut updates => {}
        }
    });
    Ok(())
}

struct Live {
    on: bool,
    input: u32,
    brightness: Option<i32>,
    art: Option<bool>,
    reachable: bool,
    seen: bool,
}

async fn run(
    mut device: FrameDevice,
    mut commands: mpsc::Receiver<Command>,
    states: watch::Sender<View>,
) -> Result<()> {
    let mut live = Live {
        on: false,
        input: 0,
        brightness: None,
        art: None,
        reachable: false,
        seen: false,
    };
    let mut poll = tokio::time::interval(Duration::from_secs(20));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                if let Err(error) = handle(&mut device, &mut live, command).await {
                    log::warn!("{}: {error:#}", device.name);
                }
                publish(&states, &device, &live);
            }
            _ = poll.tick() => {
                if let Err(error) = refresh(&mut device, &mut live).await {
                    log::debug!("{}: {error:#}", device.name);
                    live.reachable = false;
                    live.seen = true;
                    live.on = false;
                }
                publish(&states, &device, &live);
            }
        }
    }
    Ok(())
}

fn publish(states: &watch::Sender<View>, device: &FrameDevice, live: &Live) {
    states
        .send(View {
            on: live.on,
            input: live.input,
            brightness: live.brightness,
        })
        .ok();
    crate::status::update(frame_snap(device, live));
}

fn frame_snap(device: &FrameDevice, live: &Live) -> crate::status::DeviceSnap {
    let headline = if !live.seen {
        "Looking"
    } else if !live.reachable {
        "Unreachable"
    } else if !live.on {
        "Standby"
    } else if live.art == Some(true) || live.input == Input::Art as u32 {
        "Art"
    } else if let Some(input) = Input::from_id(live.input) {
        input.label()
    } else {
        "On"
    };
    let mut metrics = Vec::new();
    if live.seen && live.reachable {
        metrics.push(crate::status::Metric {
            label: "Power".into(),
            value: if live.on { "On" } else { "Standby" }.into(),
        });
    }
    if let Some(level) = live.brightness {
        metrics.push(crate::status::Metric {
            label: "Art light".into(),
            value: level.to_string(),
        });
    }
    crate::status::DeviceSnap {
        id: format!("frame:{}", device.host),
        kind: "frame".into(),
        name: device.name.clone(),
        online: live.reachable,
        headline: headline.into(),
        detail: device.model.clone(),
        metrics,
        seen: if live.seen {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0)
        } else {
            0
        },
    }
}

async fn refresh(device: &mut FrameDevice, live: &mut Live) -> Result<()> {
    let info = link::device_info(&device.host).await?;
    live.on = info.on;
    live.reachable = true;
    live.seen = true;
    if device.mac.is_empty() && !info.mac.is_empty() {
        device.mac = info.mac.clone();
        remember(device);
    }
    if device.model.is_empty() && !info.model.is_empty() {
        device.model = info.model.clone();
        remember(device);
    }
    if !info.on || device.token.is_empty() {
        return Ok(());
    }
    let replies = link::art(
        device.host.as_str(),
        &device.token,
        "get_artmode_status",
        None,
    )
    .await?;
    for reply in replies {
        match reply {
            ArtReply::Mode(on) => {
                live.art = Some(on);
                if on {
                    live.input = Input::Art as u32;
                } else if live.input == Input::Art as u32 || live.input == 0 {
                    live.input = Input::Hdmi1 as u32;
                }
            }
            ArtReply::Brightness(level) => {
                live.brightness = Some(protocol::home_brightness(level));
            }
        }
    }
    Ok(())
}

async fn handle(device: &mut FrameDevice, live: &mut Live, command: Command) -> Result<()> {
    match command {
        Command::Active(true) => {
            if !live.on {
                if !device.mac.is_empty() {
                    link::wake(&device.mac)?;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                if link::device_info(&device.host)
                    .await
                    .map(|info| info.on)
                    .unwrap_or(false)
                {
                    live.on = true;
                } else {
                    click(device, "KEY_POWER").await?;
                    live.on = true;
                }
            }
        }
        Command::Active(false) => {
            click(device, "KEY_POWEROFF").await?;
            live.on = false;
            live.art = Some(false);
        }
        Command::Input(id) => {
            let Some(input) = Input::from_id(id) else {
                return Ok(());
            };
            live.input = id;
            live.on = true;
            match input {
                Input::Art => set_art(device, live, true).await?,
                other => {
                    let _ = set_art(device, live, false).await;
                    if let Some(key) = other.key() {
                        click(device, key).await?;
                    }
                }
            }
        }
        Command::Key(value) => {
            if let Some(key) = protocol::hap_remote_key(value) {
                click(device, key).await?;
            }
        }
        Command::Brightness(home) => {
            let level = protocol::art_brightness(home);
            live.brightness = Some(protocol::home_brightness(level));
            let replies = link::art(
                &device.host,
                &device.token,
                "set_brightness",
                Some(&level.to_string()),
            )
            .await?;
            if replies.is_empty() {
                log::info!("the TV did not confirm the art brightness");
            }
        }
        Command::Volume(0) => click(device, "KEY_VOLUP").await?,
        Command::Volume(_) => click(device, "KEY_VOLDOWN").await?,
        Command::Mute => click(device, "KEY_MUTE").await?,
    }
    Ok(())
}

async fn set_art(device: &mut FrameDevice, live: &mut Live, on: bool) -> Result<()> {
    let value = if on { "on" } else { "off" };
    let replies = link::art(
        device.host.as_str(),
        &device.token,
        "set_artmode_status",
        Some(value),
    )
    .await
    .unwrap_or_default();
    let confirmed = replies
        .iter()
        .any(|reply| matches!(reply, ArtReply::Mode(mode) if *mode == on));
    if confirmed || !replies.is_empty() {
        live.art = Some(on);
        return Ok(());
    }
    // The 2024 art channel sometimes accepts nothing. One power press moves a
    // Frame that is showing a source into art, and one press in art leaves it.
    if live.on && live.art != Some(on) {
        click(device, "KEY_POWER").await?;
        live.art = Some(on);
        if !on {
            live.input = Input::Hdmi1 as u32;
        }
    } else if !live.on && on {
        link::wake(&device.mac).ok();
        tokio::time::sleep(Duration::from_secs(3)).await;
        click(device, "KEY_POWER").await?;
        live.on = true;
        live.art = Some(true);
    }
    Ok(())
}

async fn click(device: &mut FrameDevice, key: &str) -> Result<()> {
    if device.token.is_empty() {
        anyhow::bail!("not paired. Run `hearth frame pair` while the TV is on");
    }
    if let Some(token) = link::click(&device.host, &device.token, key).await? {
        if token != device.token {
            device.token = token;
            remember(device);
        }
    }
    Ok(())
}

fn remember(device: &FrameDevice) {
    let Ok(mut config) = config::load() else {
        return;
    };
    let Some(config::Device::Frame(saved)) = config
        .devices
        .iter_mut()
        .find(|item| matches!(item, config::Device::Frame(tv) if tv.host == device.host))
    else {
        return;
    };
    saved.token = device.token.clone();
    saved.mac = device.mac.clone();
    saved.model = device.model.clone();
    if let Err(error) = config::save(&config) {
        log::warn!("could not save the TV token: {error:#}");
    }
}
