//! Adding a device on an MQTT broker: where the broker is, then the topics
//! for what it is. Anything more goes in its table in config.toml.

use super::settings::{Category, Control, ControlKind, MqttDevice};
use crate::{
    capabilities::{Item, Role},
    config,
    setup::{answer, given, Answers, Field, Next, Step},
};
use anyhow::{bail, Result};

pub enum Setup {
    Start,
    Topics(MqttDevice),
}

impl Setup {
    pub fn start() -> (Self, Step) {
        let kinds = vec![
            Item::new("switch", "Switch"),
            Item::new("light", "Light"),
            Item::new("sensor", "Sensor"),
        ];
        let step = Step::new(
            "Where it is",
            "The MQTT broker the device talks through, like Mosquitto beside Zigbee2MQTT, \
             Tasmota or ESPHome.",
        )
        .field(Field::text("name", "Name in Home"))
        .field(Field::choice("category", "What it is", kinds).value("switch"))
        .field(Field::text("host", "Broker address"))
        .field(Field::text("port", "Broker port").value("1883"))
        .field(Field::text("username", "User name").optional())
        .field(Field::secret("password", "Password").optional());
        (Self::Start, step)
    }
}

impl crate::driver::Setup for Setup {
    async fn next(&mut self, answers: &Answers) -> Result<Next> {
        match self {
            Self::Start => {
                let name = answer(answers, "name")?;
                let category = match given(answers, "category").as_deref() {
                    Some("light") => Category::Light,
                    Some("sensor") => Category::Sensor,
                    _ => Category::Switch,
                };
                let port = match given(answers, "port") {
                    Some(port) => port.parse()?,
                    None => 1883,
                };
                let device = MqttDevice {
                    key: key(&name),
                    name,
                    category,
                    host: answer(answers, "host")?,
                    port,
                    username: given(answers, "username").unwrap_or_default(),
                    password: given(answers, "password").unwrap_or_default(),
                    ..MqttDevice::default()
                };
                let step = topics_step(category);
                *self = Self::Topics(device);
                Ok(Next::Ask(step))
            }
            Self::Topics(device) => {
                let mut device = device.clone();
                device.controls = controls(device.category, answers)?;
                let mut saved = config::load_or_new()?;
                config::upsert(
                    &mut saved,
                    config::Device::new::<super::Mqtt>(device.clone()),
                );
                config::save(&saved)?;
                Ok(Next::Done(format!(
                    "Added {}. Its controls are in config.toml, under key = \"{}\", for more.",
                    device.name, device.key
                )))
            }
        }
    }
}

/// An id from a name: `Desk lamp` is `desk-lamp`.
fn key(name: &str) -> String {
    let words: Vec<String> = name
        .split(|letter: char| !letter.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    words.join("-")
}

fn topics_step(category: Category) -> Step {
    let step = Step::new(
        "Its topics",
        "Where it reports and where it is set. With a field, messages are JSON and the value is \
         that field, as Zigbee2MQTT sends them.",
    )
    .field(Field::text("state", "Topic it reports on").hint("zigbee2mqtt/desk-lamp"));
    match category {
        Category::Sensor => step
            .field(
                Field::text("temperature", "Temperature field")
                    .value("temperature")
                    .optional(),
            )
            .field(
                Field::text("humidity", "Humidity field")
                    .value("humidity")
                    .optional(),
            ),
        _ => {
            let step = step
                .field(
                    Field::text("command", "Topic it is set on").hint("zigbee2mqtt/desk-lamp/set"),
                )
                .field(Field::text("field", "Field").value("state").optional())
                .field(Field::text("on", "Says for on").value("ON"))
                .field(Field::text("off", "Says for off").value("OFF"));
            if category == Category::Light {
                step.field(
                    Field::text("brightness", "Brightness field")
                        .value("brightness")
                        .optional(),
                )
                .field(
                    Field::text("brightest", "Brightest")
                        .value("254")
                        .optional(),
                )
            } else {
                step
            }
        }
    }
}

fn controls(category: Category, answers: &Answers) -> Result<Vec<Control>> {
    let state = answer(answers, "state")?;
    let base = Control {
        state: state.clone(),
        command: given(answers, "command").unwrap_or_default(),
        on: given(answers, "on").unwrap_or_else(|| "ON".into()),
        off: given(answers, "off").unwrap_or_else(|| "OFF".into()),
        ..Control::default()
    };
    let mut controls = Vec::new();
    if category == Category::Sensor {
        for (id, role, unit) in [
            ("temperature", Role::Temperature, "°C"),
            ("humidity", Role::Humidity, "%"),
        ] {
            if let Some(field) = given(answers, id) {
                controls.push(Control {
                    id: id.into(),
                    kind: ControlKind::Reading,
                    role,
                    field,
                    unit: unit.into(),
                    command: String::new(),
                    ..base.clone()
                });
            }
        }
        if controls.is_empty() {
            bail!("a sensor needs a temperature or a humidity field");
        }
        return Ok(controls);
    }
    if base.command.is_empty() {
        bail!("the topic it is set on is needed");
    }
    controls.push(Control {
        kind: ControlKind::Power,
        field: given(answers, "field").unwrap_or_default(),
        ..base.clone()
    });
    if let (Category::Light, Some(field)) = (category, given(answers, "brightness")) {
        let brightest = given(answers, "brightest")
            .map(|text| text.parse())
            .transpose()?;
        controls.push(Control {
            id: "brightness".into(),
            kind: ControlKind::Level,
            role: Role::Brightness,
            field,
            max: brightest.or(Some(254.0)),
            ..base
        });
    }
    Ok(controls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_light_from_its_answers() {
        assert_eq!(key("Desk lamp (left)"), "desk-lamp-left");
        let answers: Answers = [
            ("state", "zigbee2mqtt/lamp"),
            ("command", "zigbee2mqtt/lamp/set"),
            ("field", "state"),
            ("brightness", "brightness"),
            ("brightest", "254"),
        ]
        .into_iter()
        .map(|(id, value)| (id.to_owned(), value.to_owned()))
        .collect();
        let light = controls(Category::Light, &answers).unwrap();
        assert_eq!(light.len(), 2);
        assert_eq!(light[0].kind, ControlKind::Power);
        assert_eq!(light[1].max, Some(254.0));
        assert!(controls(Category::Sensor, &answers).is_err());
    }
}
