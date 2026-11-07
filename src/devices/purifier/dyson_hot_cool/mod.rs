//! Dyson's Purifier Hot+Cool line (HP04, HP07, HP09; product types 527,
//! 527E, 527K): one task that holds the fan's MQTT session and state,
//! published as capabilities. What every Dyson shares (the account, finding
//! a fan, how messages are framed) is in `connectors::dyson`; what this
//! line's fields mean is here, in `protocol`.

mod capabilities;
mod protocol;
mod settings;
mod setup;

pub use settings::DysonHotCoolDevice;

use crate::connectors::dyson::{
    discover,
    messages::{
        command_topic, request_environment_payload, request_state_payload, state_set_payload,
        status_topic,
    },
};
use crate::connectors::mqtt;
use crate::devices::runtime;
use anyhow::{anyhow, Result};
use protocol::{intent_fields, Intent, MachineState, DEFAULT_PRODUCT_TYPES};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// Dyson purifiers, over their own MQTT on the local network.
pub struct DysonHotCool;

crate::register_driver!(DysonHotCool);

impl crate::driver::Driver for DysonHotCool {
    const KIND: &'static str = "dyson-hot-cool";
    const ALIASES: &'static [&'static str] = &["dyson"];
    const NAME: &'static str = "Dyson Purifier Hot+Cool";
    const CATEGORY: &'static str = "purifier";
    type Settings = DysonHotCoolDevice;
    type Setup = setup::Setup;

    fn setup() -> (Self::Setup, crate::setup::Step) {
        Self::Setup::start()
    }

    async fn start(settings: DysonHotCoolDevice, context: crate::driver::Context) -> Result<()> {
        spawn(settings, context).await
    }
}

async fn spawn(device: DysonHotCoolDevice, context: crate::driver::Context) -> Result<()> {
    let shown = device.clone();
    let states = runtime::published(&context, MachineState::default(), move |state| {
        capabilities::snapshot(&shown, state)
    });
    // Every surface asks in the same intents, refused while the fan is not
    // connected rather than sent when it comes back.
    let (intents, intent_rx) = mpsc::channel::<Intent>(32);
    let known = states.clone();
    runtime::answer(context.requests(), move |request| {
        let decided = if known.borrow().connected {
            capabilities::intent(request)
        } else {
            Err(anyhow!(
                "the purifier is not connected, so nothing was sent"
            ))
        };
        let intents = intents.clone();
        async move {
            intents
                .send(decided?)
                .await
                .map_err(|_| anyhow!("the fan stopped"))
        }
    });
    tokio::spawn(async move {
        if let Err(error) = drive(device, states, intent_rx, context).await {
            log::error!("fan stopped: {error:#}");
        }
    });
    Ok(())
}

fn product_types(device: &DysonHotCoolDevice) -> Vec<String> {
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

/// Where the fan is: its saved address while it answers there, else where
/// it says it is on the network. A router hands a fan that was away a new
/// address, and may give its old one to something else.
async fn resolve_host(saved: &str, device: &DysonHotCoolDevice) -> Result<String> {
    let saved = saved.trim();
    if !saved.is_empty() && answers(saved, device.port).await {
        return Ok(saved.to_owned());
    }
    log::info!("looking for {} on the network", device.name);
    match discover::find_host(&device.serial, Duration::from_secs(12)).await {
        Ok(found) => Ok(found),
        // Not heard: the saved address, for the error it gives.
        Err(_) if !saved.is_empty() => Ok(saved.to_owned()),
        Err(error) => Err(error),
    }
}

/// Whether something takes a connection at `host`:`port` within 3 s.
async fn answers(host: &str, port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::TcpStream::connect((host, port))
        )
        .await,
        Ok(Ok(_))
    )
}

async fn drive(
    device: DysonHotCoolDevice,
    states: watch::Sender<MachineState>,
    mut intents: mpsc::Receiver<Intent>,
    context: crate::driver::Context,
) -> Result<()> {
    // Where it was last found, kept when it moves.
    let saved = std::sync::Arc::new(std::sync::Mutex::new(device.host.clone()));
    let (command_tx, mut command_rx) = mpsc::channel::<mqtt::Publish>(32);
    let (event_tx, mut event_rx) = mpsc::channel::<mqtt::Incoming>(32);
    let topics: Vec<String> = product_types(&device)
        .iter()
        .map(|kind| status_topic(kind, &device.serial))
        .collect();
    let hello = hello(&device);
    let connection = async {
        let offline = |state: &mut MachineState| state.connected = false;
        runtime::keep_connected(
            &device.name,
            &states,
            offline,
            &mut command_rx,
            |commands| {
                let (device, topics, hello, events, states, saved, context) = (
                    device.clone(),
                    topics.clone(),
                    hello.clone(),
                    event_tx.clone(),
                    states.clone(),
                    saved.clone(),
                    context.clone(),
                );
                Box::pin(async move {
                    // Looked for again each time, as its address may have changed.
                    let was = saved
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .clone();
                    let host = resolve_host(&was, &device).await?;
                    if !was.is_empty() && host != was {
                        log::info!("{} moved to a new address; remembering it", device.name);
                        *saved.lock().unwrap_or_else(|poison| poison.into_inner()) = host.clone();
                        let new = host.clone();
                        context.remember(move |kept: &mut DysonHotCoolDevice| kept.host = new);
                    }
                    log::info!("{} is at {host}:{}", device.name, device.port);
                    let broker = mqtt::Broker {
                        host: &host,
                        port: device.port,
                        username: &device.serial,
                        password: &device.mqtt_password,
                    };
                    // The fan is its own broker: taken is connected.
                    let connected = move || states.send_modify(|state| state.connected = true);
                    mqtt::session(&broker, &topics, &hello, commands, &events, connected).await
                })
            },
        )
        .await;
    };
    tokio::select! {
        () = connection => Ok(()),
        result = fan(&device, &states, &mut intents, &mut event_rx, &command_tx) => result,
    }
}

/// What a new session asks the fan at once: its state and its readings, on
/// each topic it may use.
fn hello(device: &DysonHotCoolDevice) -> Vec<mqtt::Publish> {
    product_types(device)
        .iter()
        .flat_map(|kind| {
            [request_state_payload(), request_environment_payload()].map(|payload| mqtt::Publish {
                topic: command_topic(kind, &device.serial),
                payload,
            })
        })
        .collect()
}

/// The fan as hearth keeps it: its state, in `states`, from what it reports
/// on `events`; intents sent to it on `commands`; and its state asked for
/// every half a minute. A session asks at once (see `hello`). The MQTT
/// session is on the other side of the channels, or a test.
async fn fan(
    device: &DysonHotCoolDevice,
    states: &watch::Sender<MachineState>,
    intent_rx: &mut mpsc::Receiver<Intent>,
    event_rx: &mut mpsc::Receiver<mqtt::Incoming>,
    command_tx: &mpsc::Sender<mqtt::Publish>,
) -> Result<()> {
    let types = product_types(device);
    let serial = device.serial.clone();
    let mut prefix = if types.len() == 1 {
        Some(types[0].clone())
    } else {
        None
    };
    let every = Duration::from_secs(30);
    let mut poll = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            incoming = event_rx.recv() => {
                let Some(incoming) = incoming else { break };
                log::debug!("fan: {}", incoming.payload);
                states.send_if_modified(|state| {
                    protocol::apply_message(state, &incoming.topic, &incoming.payload)
                });
                if let Some(said) = states.borrow().topic_prefix.clone() {
                    prefix = Some(said);
                }
            }
            intent = intent_rx.recv() => {
                let Some(intent) = intent else { break };
                // Shown at once. The fan confirms a moment later, and until
                // then the menu bar would put a switch back where it was.
                let state = states.borrow().clone();
                if let Some(fields) = publish(command_tx, &serial, prefix.as_deref(), &state, &intent).await {
                    states.send_modify(|state| protocol::assume(state, &fields));
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
            payload: state_set_payload(&fields),
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
        fn device() -> DysonHotCoolDevice {
            DysonHotCoolDevice {
                name: "Purifier".into(),
                serial: SERIAL.into(),
                model: "HP09".into(),
                product_type: "527K".into(),
                mqtt_password: "secret".into(),
                host: "10.0.0.7".into(),
                port: 1883,
                heating: true,
            }
        }

        fn start() -> Self {
            let device = Self::device();
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

    #[test]
    fn a_session_asks_the_fan_for_its_state_and_readings_at_once() {
        let hello = hello(&Fake::device());
        assert!(hello
            .iter()
            .all(|ask| ask.topic == command_topic("527K", SERIAL)));
        assert!(hello[0].payload.contains("REQUEST-CURRENT-STATE"));
        assert!(hello[1]
            .payload
            .contains("REQUEST-PRODUCT-ENVIRONMENT-CURRENT-SENSOR-DATA"));
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
    async fn home_shows_a_purifier_with_its_sensors_heater_and_switches() {
        use crate::surfaces::homekit::Home;
        use hap::HapType;
        use serde_json::json;
        let device = DysonHotCoolDevice {
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
        let mut home = Home::of(&snapshot).await;
        for kind in [
            HapType::AirPurifier,
            HapType::AirQualitySensor,
            HapType::HumiditySensor,
            HapType::HeaterCooler,
            HapType::FilterMaintenance,
        ] {
            assert_eq!(home.services(2, kind).len(), 1, "{kind:?}");
        }
        // Night mode, jet focus and monitoring. Its sweeps are left to the
        // menu bar: swing is Home's own.
        assert_eq!(home.services(2, HapType::Switch).len(), 3);
        let purifier = home.services(2, HapType::AirPurifier)[0];
        assert_eq!(
            home.write(2, purifier, HapType::Active, json!(1)).await,
            [("power", json!(true))]
        );
    }
}
