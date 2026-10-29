//! A Dyson purifier: one task that holds the fan's MQTT session and state,
//! and shows it to HomeKit and as capabilities.

mod accessory;
mod capabilities;

use crate::{
    config::DysonDevice,
    connectors::dyson::{
        discover, mqtt,
        protocol::{
            self, command_topic, intent_fields, request_environment_payload, request_state_payload,
            status_topic, Intent, MachineState, DEFAULT_PRODUCT_TYPES,
        },
    },
};
use accessory::{apply_purifier, build_purifier};
use anyhow::{anyhow, Result};
use hap::server::{IpServer, Server};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

pub async fn spawn(device: DysonDevice, server: IpServer) -> Result<()> {
    let (intent_tx, intent_rx) = mpsc::channel::<Intent>(32);
    // The menu bar and the CLI ask through the registry, in the same intents
    // HomeKit sends.
    let snapshot = capabilities::snapshot(&device, &MachineState::default());
    let id = snapshot.id.clone();
    crate::capabilities::publish(snapshot);
    let (requests_tx, mut requests) = mpsc::channel::<crate::capabilities::Set>(16);
    crate::capabilities::listen(&id, requests_tx);
    let intents = intent_tx.clone();
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            let result = match capabilities::intent(&request) {
                Ok(intent) => intents
                    .send(intent)
                    .await
                    .map_err(|_| anyhow!("the fan stopped")),
                Err(error) => Err(error),
            };
            request.finish(result);
        }
    });
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
            result = drive(device, state_tx, intent_rx) => {
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
    state_tx: watch::Sender<MachineState>,
    mut intent_rx: mpsc::Receiver<Intent>,
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
                log::debug!("fan: {}", incoming.payload);
                if protocol::apply_message(&mut state, &incoming.topic, &incoming.payload) {
                    if state.topic_prefix.is_some() {
                        prefix = state.topic_prefix.clone();
                    }
                    state_tx.send(state.clone()).ok();
                    crate::capabilities::publish(capabilities::snapshot(&device, &state));
                }
            }
            intent = intent_rx.recv() => {
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
    Ok(())
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
    log::info!("fan: {intent:?} -> {fields:?}");
    let _ = commands
        .send(mqtt::Publish {
            topic: command_topic(prefix, serial),
            payload: protocol::state_set_payload(&fields),
        })
        .await;
}
