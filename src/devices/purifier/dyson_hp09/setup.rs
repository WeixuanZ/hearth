//! Adding a Dyson purifier: a MyDyson sign-in with an emailed code, once.

use crate::connectors::dyson::{
    cloud::{self, Challenge, CloudDevice},
    product_name,
};
use crate::{
    capabilities::Item,
    config,
    devices::purifier::dyson_hp09::DysonHp09Device,
    setup::{answer, Answers, Field, Next, Step},
};
use anyhow::{anyhow, Result};

pub enum Setup {
    Account,
    Code(Challenge),
    Choose(Vec<CloudDevice>),
}

impl Setup {
    pub fn start() -> (Self, Step) {
        let step = Step::new(
            "Sign in to MyDyson",
            "Once, to learn the fan's local password. After that hearth talks only to the fan.",
        )
        .field(Field::text("email", "Email").hint("you@example.com"))
        .field(
            Field::text("country", "Country")
                .value("GB")
                .hint("Two letters, like GB"),
        );
        (Self::Account, step)
    }
}

impl crate::driver::Setup for Setup {
    async fn next(&mut self, answers: &Answers) -> Result<Next> {
        match self {
            Self::Account => {
                let email = answer(answers, "email")?;
                let challenge = cloud::request_code(&email, &answer(answers, "country")?).await?;
                *self = Self::Code(challenge);
                Ok(Next::Ask(
                    Step::new(
                        "Enter the code Dyson emailed",
                        format!(
                            "Dyson has emailed a code to {email}. The password is used once, not kept."
                        ),
                    )
                    .field(Field::secret("password", "MyDyson password"))
                    .field(Field::text("code", "Email code")),
                ))
            }
            Self::Code(challenge) => {
                let password = answer(answers, "password")?;
                let fans = cloud::verify(challenge, &password, &answer(answers, "code")?).await?;
                match fans.as_slice() {
                    [] => Err(anyhow!("the MyDyson account has no fan")),
                    [fan] => add(fan),
                    many => {
                        let options = many
                            .iter()
                            .map(|fan| {
                                Item::new(&fan.serial, format!("{} ({})", fan.name, fan.model))
                            })
                            .collect();
                        *self = Self::Choose(fans);
                        Ok(Next::Ask(
                            Step::new("Choose the fan", "The account has more than one.")
                                .field(Field::choice("serial", "Fan", options)),
                        ))
                    }
                }
            }
            Self::Choose(fans) => {
                let serial = answer(answers, "serial")?;
                let fan = fans
                    .iter()
                    .find(|fan| fan.serial.eq_ignore_ascii_case(&serial))
                    .ok_or_else(|| anyhow!("no fan on the account has serial {serial}"))?;
                add(fan)
            }
        }
    }
}

fn add(fan: &CloudDevice) -> Result<Next> {
    let name = product_name(&fan.model);
    let mut saved = config::load_or_new()?;
    config::upsert(
        &mut saved,
        config::Device::new::<crate::devices::purifier::dyson_hp09::DysonHp09>(DysonHp09Device {
            aid: 0,
            name: name.clone(),
            serial: fan.serial.clone(),
            model: fan.model.clone(),
            product_type: fan.product_type.clone(),
            mqtt_password: fan.mqtt_password.clone(),
            host: String::new(),
            port: 1883,
            heating: true,
        }),
    );
    config::save(&saved)?;
    Ok(Next::Done(format!("Added {name}.")))
}
