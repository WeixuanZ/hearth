//! Any device on an MQTT broker, described in the config: a light, a
//! switch or a sensor, from Zigbee2MQTT, Tasmota, ESPHome or a script of
//! your own. Each control says the topic it reports on and the topic it is
//! set on; hearth makes capabilities of them, and so the menu bar, the CLI,
//! agents and Home show it with no code of its own.

mod capabilities;
mod settings;
mod setup;

pub use settings::MqttDevice;

use crate::{connectors::mqtt, devices::runtime};
use anyhow::{anyhow, Result};
use capabilities::State;
use tokio::sync::mpsc;

/// Devices on an MQTT broker.
pub struct Mqtt;

crate::register_driver!(Mqtt);

impl crate::driver::Driver for Mqtt {
    const KIND: &'static str = "mqtt";
    const NAME: &'static str = "MQTT device";
    const CATEGORY: &'static str = "switch";
    type Settings = MqttDevice;
    type Setup = setup::Setup;

    fn setup() -> (Self::Setup, crate::setup::Step) {
        setup::Setup::start()
    }

    async fn start(settings: MqttDevice, context: crate::driver::Context) -> Result<()> {
        spawn(settings, context).await
    }
}

async fn spawn(device: MqttDevice, context: crate::driver::Context) -> Result<()> {
    let shown = device.clone();
    let states = runtime::published(&context, State::default(), move |state| {
        capabilities::snapshot(&shown, state)
    });

    // Requests become messages, refused while the broker is not connected.
    let (commands_tx, mut commands) = mpsc::channel::<mqtt::Publish>(32);
    let (known, asked) = (states.clone(), device.clone());
    runtime::answer(context.requests(), move |request| {
        let decided = if known.borrow().connected {
            capabilities::command(&asked, request)
        } else {
            Err(anyhow!(
                "{} is not connected to its broker, so nothing was sent",
                asked.name
            ))
        };
        let (commands, states) = (commands_tx.clone(), known.clone());
        async move {
            let (publish, id, shown) = decided?;
            commands
                .send(publish)
                .await
                .map_err(|_| anyhow!("the connection stopped"))?;
            // Shown at once; what the device reports has the last word.
            states.send_modify(|state| {
                state.values.insert(id, shown);
            });
            Ok(())
        }
    });

    // What it says, as state.
    let (events_tx, mut events) = mpsc::channel::<mqtt::Incoming>(64);
    let (heard, said) = (states.clone(), device.clone());
    tokio::spawn(async move {
        while let Some(incoming) = events.recv().await {
            heard.send_if_modified(|state| {
                capabilities::heard(&said, state, &incoming.topic, &incoming.payload)
            });
        }
    });

    let topics = device.topics();
    tokio::spawn(async move {
        let offline = |state: &mut State| state.connected = false;
        runtime::keep_connected(&device.name, &states, offline, &mut commands, |commands| {
            let (device, topics, events, states) = (
                device.clone(),
                topics.clone(),
                events_tx.clone(),
                states.clone(),
            );
            Box::pin(async move {
                let broker = mqtt::Broker {
                    host: &device.host,
                    port: device.port,
                    username: &device.username,
                    password: &device.password,
                };
                let connected = move || states.send_modify(|state| state.connected = true);
                mqtt::session(&broker, &topics, &[], commands, &events, connected).await
            })
        })
        .await;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::settings::{Category, Control, ControlKind};
    use super::*;
    use crate::{capabilities::Role, surfaces::homekit::Home};
    use hap::HapType;
    use serde_json::json;

    fn control(id: &str, kind: ControlKind, role: Role) -> Control {
        Control {
            id: id.into(),
            kind,
            role,
            state: "thing/state".into(),
            command: "thing/set".into(),
            field: id.into(),
            on: "ON".into(),
            off: "OFF".into(),
            ..Control::default()
        }
    }

    fn device(category: Category, controls: Vec<Control>) -> MqttDevice {
        MqttDevice {
            key: "thing".into(),
            name: "Thing".into(),
            category,
            host: "10.0.0.5".into(),
            port: 1883,
            controls,
            ..MqttDevice::default()
        }
    }

    #[tokio::test]
    async fn a_light_is_a_lightbulb_in_home_over_its_own_range() {
        let mut brightness = control("brightness", ControlKind::Level, Role::Brightness);
        brightness.max = Some(254.0);
        let lamp = device(
            Category::Light,
            vec![
                control("power", ControlKind::Power, Role::Generic),
                brightness,
            ],
        );
        let snapshot = capabilities::snapshot(&lamp, &State::default());
        let mut home = Home::of(&snapshot).await;
        let light = home.services(2, HapType::Lightbulb);
        assert_eq!(light.len(), 1);
        assert_eq!(
            home.write(2, light[0], HapType::PowerState, json!(true))
                .await,
            [("power", json!(true))]
        );
        assert_eq!(
            home.write(2, light[0], HapType::Brightness, json!(50))
                .await,
            [("brightness", json!(127.0))]
        );
    }

    #[tokio::test]
    async fn a_sensor_is_home_s_sensors() {
        let sensor = device(
            Category::Sensor,
            vec![
                control("temperature", ControlKind::Reading, Role::Temperature),
                control("humidity", ControlKind::Reading, Role::Humidity),
                control("door", ControlKind::State, Role::Contact),
            ],
        );
        let snapshot = capabilities::snapshot(&sensor, &State::default());
        let home = Home::of(&snapshot).await;
        assert_eq!(home.services(2, HapType::TemperatureSensor).len(), 1);
        assert_eq!(home.services(2, HapType::HumiditySensor).len(), 1);
        assert!(home.services(2, HapType::Switch).is_empty());
        // The door notifies, so it is a tile of its own.
        assert_eq!(home.services(3, HapType::ContactSensor).len(), 1);
    }

    #[test]
    fn its_config_reads_as_written() {
        let text = r#"
            kind = "mqtt"
            key = "desk-lamp"
            name = "Desk lamp"
            category = "light"
            host = "10.0.0.5"

            [[control]]
            kind = "power"
            state = "zigbee2mqtt/desk-lamp"
            command = "zigbee2mqtt/desk-lamp/set"
            field = "state"

            [[control]]
            id = "brightness"
            kind = "level"
            role = "brightness"
            state = "zigbee2mqtt/desk-lamp"
            command = "zigbee2mqtt/desk-lamp/set"
            field = "brightness"
            max = 254
        "#;
        let mut table: toml::Table = toml::from_str(text).unwrap();
        table.remove("kind");
        let lamp: MqttDevice = toml::Value::Table(table).try_into().unwrap();
        assert_eq!(lamp.port, 1883);
        assert_eq!(lamp.controls[0].on, "ON");
        assert_eq!(lamp.controls[1].role, Role::Brightness);
        assert_eq!(lamp.topics(), ["zigbee2mqtt/desk-lamp"]);
        let written = toml::to_string(&lamp).unwrap();
        assert!(
            !written.contains("on = "),
            "defaults are not written:\n{written}"
        );
        assert_eq!(toml::from_str::<MqttDevice>(&written).unwrap(), lamp);
    }

    #[test]
    fn the_readme_example_is_a_device() {
        let readme = include_str!("../../../../README.md");
        let from = readme.find("**Anything on MQTT.**").unwrap();
        let block = &readme[from..];
        let start = block.find("```toml\n").unwrap() + "```toml\n".len();
        let end = start + block[start..].find("```").unwrap();
        let mut devices: toml::Table = toml::from_str(&block[start..end]).unwrap();
        let toml::Value::Array(devices) = devices.remove("devices").unwrap() else {
            panic!("no devices");
        };
        let toml::Value::Table(mut table) = devices[0].clone() else {
            panic!("not a table");
        };
        assert_eq!(table.remove("kind").unwrap().as_str(), Some("mqtt"));
        let lamp: MqttDevice = toml::Value::Table(table).try_into().unwrap();
        assert_eq!(lamp.controls.len(), 2);
        assert_eq!(lamp.category, Category::Light);
    }

    #[tokio::test]
    async fn a_role_that_does_not_fit_its_kind_is_left_out_not_a_crash() {
        // A light whose brightness is a reading, and a TV-like input that is
        // a switch: Home shows what fits, and the bridge stays up.
        let odd = device(
            Category::Light,
            vec![
                control("power", ControlKind::Power, Role::Generic),
                control("brightness", ControlKind::Reading, Role::Brightness),
                control("input", ControlKind::Switch, Role::Input),
            ],
        );
        let snapshot = capabilities::snapshot(&odd, &State::default());
        let home = Home::of(&snapshot).await;
        assert_eq!(home.services(2, HapType::Lightbulb).len(), 1);
    }
}
