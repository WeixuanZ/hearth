//! Dyson Purifier Hot+Cool Formaldehyde. Local MQTT, after one MyDyson login.

mod accessory;
mod cloud;
mod crypto;
mod discover;
mod mqtt;
mod protocol;

use crate::config::{self, DysonDevice};
use accessory::{apply_purifier, build_purifier};
use anyhow::{anyhow, Context, Result};
use hap::server::{IpServer, Server};
use protocol::{
    command_topic, intent_fields, request_environment_payload, request_state_payload, status_topic,
    Intent, MachineState, DEFAULT_PRODUCT_TYPES,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

pub use cloud::CloudDevice;

pub async fn login(email: String, country: String, serial: Option<String>) -> Result<()> {
    let challenge = cloud::request_code(&email, &country).await?;
    println!("Dyson has emailed a code to {email}.");
    let password = rpassword::prompt_password("MyDyson password: ")?;
    let otp = crate::prompt("Email code: ")?;
    let devices = cloud::verify(challenge, &password, &otp).await?;
    let device = choose(devices, serial)?;

    let mut saved = config::load_or_new()?;
    if saved.bridge_id.is_empty() {
        saved.bridge_id = device.serial.clone();
    }
    config::upsert_dyson(
        &mut saved,
        DysonDevice {
            aid: 0,
            name: device.name,
            serial: device.serial,
            model: device.model,
            product_type: device.product_type,
            mqtt_password: device.mqtt_password,
            host: String::new(),
            port: 1883,
            heating: true,
        },
    );
    let path = config::save(&saved)?;
    println!();
    println!("Saved {}.", path.display());
    println!("Apple Home pairing code: {}", saved.homekit_pin);
    println!("Next: hearth run");
    Ok(())
}

fn choose(devices: Vec<CloudDevice>, serial: Option<String>) -> Result<CloudDevice> {
    if let Some(serial) = serial {
        return devices
            .into_iter()
            .find(|device| device.serial.eq_ignore_ascii_case(serial.trim()))
            .ok_or_else(|| anyhow!("no fan on the account has serial {serial}"));
    }
    if devices.len() == 1 {
        return Ok(devices.into_iter().next().unwrap());
    }
    println!("This account has more than one fan:");
    for device in &devices {
        println!(
            "  {}  {}  {}  topic {}",
            device.serial, device.model, device.name, device.product_type
        );
    }
    let serial = crate::prompt("Serial to put in Apple Home: ")?;
    devices
        .into_iter()
        .find(|device| device.serial.eq_ignore_ascii_case(serial.trim()))
        .ok_or_else(|| anyhow!("that serial is not on the account"))
}

pub async fn status(device: &DysonDevice) -> Result<()> {
    drive(device.clone(), true, None).await
}

pub async fn spawn(device: DysonDevice, server: IpServer) -> Result<()> {
    let (intent_tx, intent_rx) = mpsc::channel::<Intent>(32);
    let (state_tx, state_rx) = watch::channel(MachineState::default());
    let suppress = Arc::new(AtomicBool::new(false));
    let purifier = server
        .add_accessory(build_purifier(&device, &suppress, &intent_tx).await?)
        .await
        .map_err(|error| anyhow!("{error}"))?;

    tokio::spawn(async move {
        let updates = async move {
            let mut states = state_rx;
            loop {
                let current = states.borrow().clone();
                suppress.store(true, Ordering::SeqCst);
                let result = async {
                    apply_purifier(purifier.lock().await.as_mut(), &current).await?;
                    Ok::<(), anyhow::Error>(())
                }
                .await;
                suppress.store(false, Ordering::SeqCst);
                if let Err(error) = result {
                    log::warn!("fan HomeKit update failed: {error:#}");
                }
                if states.changed().await.is_err() {
                    break;
                }
            }
        };
        tokio::pin!(updates);
        tokio::select! {
            result = drive(device, false, Some((state_tx, intent_rx))) => {
                if let Err(error) = result {
                    log::error!("fan stopped: {error:#}");
                }
            }
            _ = &mut updates => {}
        }
    });
    Ok(())
}

fn product_types(device: &DysonDevice) -> Vec<String> {
    let configured = device.product_type.trim();
    if configured.is_empty() || configured.eq_ignore_ascii_case("auto") {
        DEFAULT_PRODUCT_TYPES
            .iter()
            .map(|kind| (*kind).to_owned())
            .collect()
    } else {
        vec![configured.to_owned()]
    }
}

async fn resolve_host(device: &DysonDevice) -> Result<String> {
    if !device.host.trim().is_empty() {
        return Ok(device.host.trim().to_owned());
    }
    log::info!("looking for {} on the network", device.serial);
    discover::find_host(&device.serial, Duration::from_secs(12)).await
}

async fn drive(
    device: DysonDevice,
    status_only: bool,
    home: Option<(watch::Sender<MachineState>, mpsc::Receiver<Intent>)>,
) -> Result<()> {
    let host = resolve_host(&device).await?;
    log::info!("{} is at {host}:{}", device.name, device.port);

    let (command_tx, mut command_rx) = mpsc::channel::<mqtt::Publish>(32);
    let (event_tx, mut event_rx) = mpsc::channel::<mqtt::Incoming>(32);
    let topics: Vec<String> = product_types(&device)
        .iter()
        .map(|kind| status_topic(kind, &device.serial))
        .collect();
    let host_for_session = host.clone();
    let serial_for_session = device.serial.clone();
    let password = device.mqtt_password.clone();
    let port = device.port;
    tokio::spawn(async move {
        loop {
            if let Err(error) = mqtt::session(
                &host_for_session,
                port,
                &serial_for_session,
                &password,
                &topics,
                &mut command_rx,
                &event_tx,
            )
            .await
            {
                log::warn!("fan connection dropped: {error:#}");
            }
            if event_tx.is_closed() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });

    let (state_tx, mut intent_rx) = match home {
        Some((state_tx, intent_rx)) => (Some(state_tx), Some(intent_rx)),
        None => (None, None),
    };
    let types = product_types(&device);
    let serial = device.serial.clone();
    let mut state = MachineState::default();
    let mut prefix = if types.len() == 1 {
        Some(types[0].clone())
    } else {
        None
    };
    let mut poll = tokio::time::interval(Duration::from_secs(30));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            incoming = event_rx.recv() => {
                let Some(incoming) = incoming else { break };
                if protocol::apply_message(&mut state, &incoming.topic, &incoming.payload) {
                    if state.topic_prefix.is_some() {
                        prefix = state.topic_prefix.clone();
                    }
                    if let Some(state_tx) = &state_tx {
                        state_tx.send(state.clone()).ok();
                        crate::status::update(snap(&device, &state));
                    }
                    if status_only && state.temperature_c.is_some() {
                        print_status(&device.name, &state);
                        break;
                    }
                }
            }
            intent = recv_intent(&mut intent_rx) => {
                let Some(intent) = intent else { break };
                publish(&command_tx, &serial, prefix.as_deref(), &state, &intent).await;
            }
            _ = poll.tick() => {
                let targets: Vec<String> = match &prefix {
                    Some(prefix) => vec![prefix.clone()],
                    None => types.clone(),
                };
                for kind in targets {
                    let _ = command_tx.send(mqtt::Publish {
                        topic: command_topic(&kind, &serial),
                        payload: request_state_payload(),
                    }).await;
                    let _ = command_tx.send(mqtt::Publish {
                        topic: command_topic(&kind, &serial),
                        payload: request_environment_payload(),
                    }).await;
                }
            }
        }
    }
    if status_only && state.temperature_c.is_none() {
        anyhow::bail!("the fan did not answer");
    }
    Ok(())
}

async fn recv_intent(intents: &mut Option<mpsc::Receiver<Intent>>) -> Option<Intent> {
    match intents {
        Some(intents) => intents.recv().await,
        None => std::future::pending().await,
    }
}

async fn publish(
    commands: &mpsc::Sender<mqtt::Publish>,
    serial: &str,
    prefix: Option<&str>,
    state: &MachineState,
    intent: &Intent,
) {
    let Some(prefix) = prefix else {
        log::warn!("not sending a command until the fan has identified its topic");
        return;
    };
    let fields = intent_fields(state, intent);
    let _ = commands
        .send(mqtt::Publish {
            topic: command_topic(prefix, serial),
            payload: protocol::state_set_payload(&fields),
        })
        .await;
}

fn snap(device: &DysonDevice, state: &MachineState) -> crate::status::DeviceSnap {
    let online = state.topic_prefix.is_some() || state.temperature_c.is_some();
    let headline = if !online {
        "Looking".into()
    } else if state.power_on {
        "On".into()
    } else {
        "Off".into()
    };
    let mut parts = Vec::new();
    if state.power_on {
        parts.push(if state.auto {
            "Auto".into()
        } else if let Some(speed) = state.speed {
            format!("Speed {speed}")
        } else {
            "Manual".into()
        });
    }
    if state.oscillating {
        parts.push("swinging".into());
    }
    if state.heat_on {
        parts.push(if state.heating_now {
            "heating".into()
        } else {
            "heat on".into()
        });
    }
    if state.night {
        parts.push("night".into());
    }
    if state.focus {
        parts.push("focused".into());
    }
    let mut metrics = Vec::new();
    if let Some(temp) = state.temperature_c {
        metrics.push(crate::status::Metric {
            label: "Temp".into(),
            value: format!("{temp:.0}°"),
        });
    }
    if let Some(humidity) = state.humidity {
        metrics.push(crate::status::Metric {
            label: "Humidity".into(),
            value: format!("{humidity:.0}%"),
        });
    }
    if let Some(pm) = state.pm25 {
        metrics.push(crate::status::Metric {
            label: "PM2.5".into(),
            value: format!("{pm:.0}"),
        });
    }
    if let Some(life) = state.hepa_life {
        metrics.push(crate::status::Metric {
            label: "Filter".into(),
            value: format!("{life}%"),
        });
    }
    crate::status::DeviceSnap {
        id: format!("dyson:{}", device.serial),
        kind: "dyson".into(),
        name: device.name.clone(),
        online,
        headline,
        detail: parts.join(" · "),
        metrics,
        seen: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
    }
}

fn print_status(name: &str, state: &MachineState) {
    println!("{name}");
    println!(
        "power {}  auto {}  speed {}  swing {}  night {}  focus {}  heat {}",
        on_off(state.power_on),
        on_off(state.auto),
        state
            .speed
            .map(|speed| speed.to_string())
            .unwrap_or_else(|| "-".into()),
        on_off(state.oscillating),
        on_off(state.night),
        on_off(state.focus),
        on_off(state.heat_on),
    );
    println!(
        "temp {}  humidity {}  PM2.5 {}  PM10 {}  formaldehyde {} mg/m³  HEPA {}%",
        state
            .temperature_c
            .map(|n| format!("{n:.1}°C"))
            .unwrap_or_else(|| "-".into()),
        state
            .humidity
            .map(|n| format!("{n:.0}%"))
            .unwrap_or_else(|| "-".into()),
        state
            .pm25
            .map(|n| format!("{n:.0}"))
            .unwrap_or_else(|| "-".into()),
        state
            .pm10
            .map(|n| format!("{n:.0}"))
            .unwrap_or_else(|| "-".into()),
        state
            .hcho_mg_m3
            .map(|n| format!("{n:.3}"))
            .unwrap_or_else(|| "-".into()),
        state
            .hepa_life
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into()),
    );
}

fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

pub async fn status_timeout(device: &DysonDevice) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), status(device))
        .await
        .context("the fan did not answer within 20 seconds")?
}
