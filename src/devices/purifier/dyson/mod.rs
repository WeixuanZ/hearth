//! A Dyson purifier: one task that holds the fan's MQTT session and state,
//! and shows it to HomeKit and as capabilities.

mod accessory;
mod capabilities;
mod settings;

pub use settings::DysonDevice;

use crate::{
    connectors::dyson::{
        discover, mqtt,
        protocol::{
            self, command_topic, intent_fields, request_environment_payload, request_state_payload,
            status_topic, Intent, MachineState, DEFAULT_PRODUCT_TYPES,
        },
    },
    surfaces::homekit::characteristics::follow_name,
};
use accessory::{apply_purifier, build_purifier};
use anyhow::{anyhow, Result};
use hap::{server::Server, HapType};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

/// Dyson purifiers, over their own MQTT on the local network.
pub struct Dyson;

impl crate::driver::Driver for Dyson {
    const KIND: &'static str = "dyson";
    const NAME: &'static str = "Dyson purifier";
    const CATEGORY: &'static str = "purifier";
    type Settings = DysonDevice;
    type Setup = crate::connectors::dyson::Setup;

    fn setup() -> (Self::Setup, crate::setup::Step) {
        Self::Setup::start()
    }

    async fn start(settings: DysonDevice, context: crate::driver::Context) -> Result<()> {
        spawn(settings, context).await
    }
}

async fn spawn(device: DysonDevice, context: crate::driver::Context) -> Result<()> {
    let server = context.homekit().clone();
    let (intent_tx, intent_rx) = mpsc::channel::<Intent>(32);
    // The menu bar and the CLI ask through the registry, in the same intents
    // HomeKit sends.
    let snapshot = capabilities::snapshot(&device, &MachineState::default());
    let id = snapshot.id.clone();
    context.publish(snapshot);
    let mut requests = context.requests();
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
    follow_name(&id, purifier.clone(), HapType::AirPurifier);

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

    fan(
        &device,
        &state_tx,
        &mut intent_rx,
        &mut event_rx,
        &command_tx,
    )
    .await
}

/// The fan as hearth keeps it: state from what it reports on `events`,
/// intents sent to it on `commands`, and its state asked for every half a
/// minute. The MQTT session is on the other side of the channels, or a test.
async fn fan(
    device: &DysonDevice,
    state_tx: &watch::Sender<MachineState>,
    intent_rx: &mut mpsc::Receiver<Intent>,
    event_rx: &mut mpsc::Receiver<mqtt::Incoming>,
    command_tx: &mpsc::Sender<mqtt::Publish>,
) -> Result<()> {
    let types = product_types(device);
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
                    crate::capabilities::publish(capabilities::snapshot(device, &state));
                }
            }
            intent = intent_rx.recv() => {
                let Some(intent) = intent else { break };
                // Shown at once. The fan confirms a moment later, and until
                // then the menu bar would put a switch back where it was.
                if let Some(fields) = publish(command_tx, &serial, prefix.as_deref(), &state, &intent).await {
                    protocol::assume(&mut state, &fields);
                    state_tx.send(state.clone()).ok();
                    crate::capabilities::publish(capabilities::snapshot(device, &state));
                }
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
) -> Option<Vec<(&'static str, String)>> {
    let Some(prefix) = prefix else {
        log::warn!("not sending a command until the fan has identified its topic");
        return None;
    };
    let fields = intent_fields(state, intent);
    log::info!("fan: {intent:?} -> {fields:?}");
    let sent = commands
        .send(mqtt::Publish {
            topic: command_topic(prefix, serial),
            payload: protocol::state_set_payload(&fields),
        })
        .await;
    sent.is_ok().then_some(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SERIAL: &str = "ABC-XY-12345678";

    /// The fan's side of the session: what hearth sends it, and what it says.
    struct Fake {
        sent: mpsc::Receiver<mqtt::Publish>,
        says: mpsc::Sender<mqtt::Incoming>,
        intents: mpsc::Sender<Intent>,
        states: watch::Receiver<MachineState>,
    }

    impl Fake {
        /// Starts hearth's side against this fan, which has said nothing yet.
        fn start() -> Self {
            let device = DysonDevice {
                aid: 2,
                name: "Purifier".into(),
                serial: SERIAL.into(),
                model: "HP09".into(),
                product_type: "527K".into(),
                mqtt_password: "secret".into(),
                host: "10.0.0.7".into(),
                port: 1883,
                heating: true,
            };
            let (state_tx, states) = watch::channel(MachineState::default());
            let (intents, mut intent_rx) = mpsc::channel(4);
            let (says, mut event_rx) = mpsc::channel(4);
            let (command_tx, sent) = mpsc::channel(16);
            tokio::spawn(async move {
                fan(
                    &device,
                    &state_tx,
                    &mut intent_rx,
                    &mut event_rx,
                    &command_tx,
                )
                .await
            });
            Self {
                sent,
                says,
                intents,
                states,
            }
        }

        async fn say(&self, message: serde_json::Value) {
            let topic = status_topic("527K", SERIAL);
            let payload = message.to_string();
            self.says
                .send(mqtt::Incoming { topic, payload })
                .await
                .unwrap();
        }

        /// The next thing hearth sent that is not a request for the state.
        async fn next_command(&mut self) -> mqtt::Publish {
            loop {
                let sent = self.sent.recv().await.unwrap();
                if !sent.payload.contains("REQUEST-") {
                    return sent;
                }
            }
        }

        async fn power(&mut self) -> bool {
            self.states.changed().await.unwrap();
            self.states.borrow().power_on
        }
    }

    #[tokio::test]
    async fn it_asks_the_fan_for_its_state_at_once() {
        let mut fake = Fake::start();
        let first = fake.sent.recv().await.unwrap();
        assert_eq!(first.topic, command_topic("527K", SERIAL));
        assert!(first.payload.contains("REQUEST-CURRENT-STATE"));
    }

    #[tokio::test]
    async fn a_command_shows_at_once_and_the_fans_report_has_the_last_word() {
        let mut fake = Fake::start();
        fake.say(json!({ "msg": "CURRENT-STATE", "product-state": { "fpwr": "OFF" } }))
            .await;
        assert!(!fake.power().await);

        fake.intents.send(Intent::Power(true)).await.unwrap();
        let command = fake.next_command().await;
        assert_eq!(command.topic, command_topic("527K", SERIAL));
        assert!(command.payload.contains(r#""fpwr":"ON""#));
        assert!(fake.power().await, "shown before the fan confirms");

        // The fan refused: its report puts the switch back.
        fake.say(json!({ "msg": "STATE-CHANGE", "product-state": { "fpwr": ["ON", "OFF"] } }))
            .await;
        assert!(!fake.power().await);
    }

    #[tokio::test]
    async fn turning_off_while_heating_ends_heat_too() {
        let mut fake = Fake::start();
        fake.say(
            json!({ "msg": "CURRENT-STATE", "product-state": { "fpwr": "ON", "hmod": "HEAT" } }),
        )
        .await;
        assert!(fake.power().await);
        fake.intents.send(Intent::Power(false)).await.unwrap();
        let command = fake.next_command().await;
        assert!(command.payload.contains(r#""fpwr":"OFF""#));
        assert!(command.payload.contains(r#""hmod":"OFF""#));
    }
}
