//! A Dyson purifier: one task that holds the fan's MQTT session and state,
//! and shows it to HomeKit and as capabilities.

mod capabilities;
mod settings;

pub use settings::DysonDevice;

use crate::connectors::dyson::{
    discover, mqtt,
    protocol::{
        self, command_topic, intent_fields, request_environment_payload, request_state_payload,
        status_topic, Intent, MachineState, DEFAULT_PRODUCT_TYPES,
    },
};
use anyhow::{anyhow, Result};
use std::time::Duration;
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

    /// The fan, its sensors, heater and filter, and its switches.
    fn homekit_ids(fan: &DysonDevice) -> crate::surfaces::homekit::ids::DeviceIds {
        let ids = crate::surfaces::homekit::ids::DeviceIds::default()
            .accessory("main", fan.aid)
            .service("main", "purifier", 20)
            .service("main", "air", 40)
            .service("main", "humidity", 70)
            .service("main", "climate", 100)
            .service("main", "filter", 140)
            .service("main", "switch:night", 170)
            .service("main", "switch:jet_focus", 180)
            .service("main", "switch:monitoring", 190);
        protocol::SWEEPS
            .iter()
            .zip((200..).step_by(10))
            .fold(ids, |ids, (width, iid)| {
                ids.service("main", format!("option:sweep:{width}"), iid)
            })
    }
}

async fn spawn(device: DysonDevice, context: crate::driver::Context) -> Result<()> {
    let (intent_tx, intent_rx) = mpsc::channel::<Intent>(32);
    context.publish(capabilities::snapshot(&device, &MachineState::default()));
    // Every surface asks in the same intents.
    let mut requests = context.requests();
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            let result = match capabilities::intent(&request) {
                Ok(intent) => intent_tx
                    .send(intent)
                    .await
                    .map_err(|_| anyhow!("the fan stopped")),
                Err(error) => Err(error),
            };
            request.finish(result);
        }
    });
    // The fan's state, as it says it, shown as a snapshot.
    let (state_tx, mut state_rx) = watch::channel(MachineState::default());
    let shown = device.clone();
    tokio::spawn(async move {
        let publish = async move {
            while state_rx.changed().await.is_ok() {
                let state = state_rx.borrow_and_update().clone();
                context.publish(capabilities::snapshot(&shown, &state));
            }
        };
        tokio::select! {
            result = drive(device, state_tx, intent_rx) => {
                if let Err(error) = result {
                    log::error!("fan stopped: {error:#}");
                }
            }
            () = publish => {}
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
                }
            }
            intent = intent_rx.recv() => {
                let Some(intent) = intent else { break };
                // Shown at once. The fan confirms a moment later, and until
                // then the menu bar would put a switch back where it was.
                if let Some(fields) = publish(command_tx, &serial, prefix.as_deref(), &state, &intent).await {
                    protocol::assume(&mut state, &fields);
                    state_tx.send(state.clone()).ok();
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

    #[tokio::test]
    async fn home_keeps_the_ids_it_knows_the_purifier_by() {
        use crate::{driver::Driver, surfaces::homekit::Home};
        use hap::HapType;
        use serde_json::json;
        let device = DysonDevice {
            aid: 3,
            name: "Purifier".into(),
            serial: "ABC-XY-12345678".into(),
            model: "XY01".into(),
            product_type: "000X".into(),
            mqtt_password: "x".into(),
            host: String::new(),
            port: 1883,
            heating: true,
        };
        let snapshot = super::capabilities::snapshot(&device, &MachineState::default());
        let mut home = Home::of(&snapshot, Dyson::homekit_ids(&device)).await;
        assert_eq!(home.services(3, HapType::AirPurifier), [20]);
        assert_eq!(home.services(3, HapType::AirQualitySensor), [40]);
        assert_eq!(home.services(3, HapType::HumiditySensor), [70]);
        assert_eq!(home.services(3, HapType::HeaterCooler), [100]);
        assert_eq!(home.services(3, HapType::FilterMaintenance), [140]);
        // Night mode, jet focus and monitoring, then one switch per sweep.
        let mut switches = home.services(3, HapType::Switch);
        switches.sort();
        assert_eq!(switches[..3], [170, 180, 190]);
        let sweeps: Vec<u64> = (0..switches.len() as u64 - 3)
            .map(|index| 200 + 10 * index)
            .collect();
        assert_eq!(switches[3..], sweeps);
        assert_eq!(
            home.write(3, 20, HapType::Active, json!(1)).await,
            [("power", json!(true))]
        );
    }
}
