//! Adding a Bosch dishwasher: one Home Connect sign-in, for its local key.

use super::HomeConnectDishwasherDevice;
use crate::connectors::home_connect::cloud::{Account, Found, SignIn};
use crate::{
    capabilities::Item,
    config,
    setup::{answer, given, Answers, Field, Next, Step},
};
use anyhow::{anyhow, Result};

pub enum Setup {
    SignIn(SignIn),
    Choose {
        account: Account,
        host: Option<String>,
    },
}

impl Setup {
    pub fn start() -> (Self, Step) {
        let sign_in = SignIn::new();
        let step = Step::new(
            "Sign in to Home Connect",
            "Once, to learn the dishwasher's local key. After that hearth talks only to the \
             dishwasher. Sign in on Home Connect's page; when it is done, let the browser open \
             Hearth. Without the menu bar app, the page ends on an address starting hcauth://: \
             copy that whole address here.",
        )
        .link(sign_in.address())
        .callback("redirect")
        .field(Field::text("redirect", "Address from the browser").hint("hcauth://…"))
        .field(
            Field::text("host", "Dishwasher's address")
                .hint("Only if its name does not resolve")
                .optional(),
        );
        (Self::SignIn(sign_in), step)
    }
}

impl crate::driver::Setup for Setup {
    async fn next(&mut self, answers: &Answers) -> Result<Next> {
        match self {
            Self::SignIn(sign_in) => {
                let account = sign_in.finish(&answer(answers, "redirect")?).await?;
                let host = given(answers, "host");
                let found = dishwashers(&account)?;
                if let [dishwasher] = found.as_slice() {
                    return add(&account, dishwasher.clone(), host).await;
                }
                let options = found
                    .iter()
                    .map(|item| Item::new(&item.ha_id, item.label()))
                    .collect();
                *self = Self::Choose { account, host };
                Ok(Next::Ask(
                    Step::new("Choose the dishwasher", "The account has more than one.")
                        .field(Field::choice("appliance", "Dishwasher", options)),
                ))
            }
            Self::Choose { account, host } => {
                let id = answer(answers, "appliance")?;
                let chosen = dishwashers(account)?
                    .into_iter()
                    .find(|item| item.ha_id.eq_ignore_ascii_case(&id))
                    .ok_or_else(|| anyhow!("no dishwasher on the account has id {id}"))?;
                add(account, chosen, host.clone()).await
            }
        }
    }
}

async fn add(
    account: &Account,
    chosen: crate::connectors::home_connect::cloud::Found,
    host: Option<String>,
) -> Result<Next> {
    let paired = account.pair(&chosen, host.as_deref()).await?;
    let device = HomeConnectDishwasherDevice {
        name: if chosen.kind.is_empty() {
            format!("{} Dishwasher", paired.brand)
        } else {
            paired.name
        },
        brand: paired.brand,
        model: paired.model,
        ha_id: paired.ha_id,
        host: paired.host,
        key: paired.key,
        iv: paired.iv,
        features: paired.features,
    };
    let name = device.name.clone();
    config::add(config::Device::new::<super::HomeConnectDishwasher>(device))?;
    Ok(Next::Done(format!(
        "Added {name}. Starting a programme needs Remote Start on the dishwasher; reading it does not."
    )))
}

/// The account's dishwashers: anything else would be driven as one.
fn dishwashers(account: &Account) -> Result<Vec<Found>> {
    let (dishwashers, others): (Vec<&Found>, Vec<&Found>) = account
        .appliances
        .iter()
        .partition(|found| is_dishwasher(found));
    if dishwashers.is_empty() {
        let others: Vec<String> = others
            .iter()
            .map(|item| format!("{} ({})", item.ha_id, item.kind))
            .collect();
        anyhow::bail!(
            "the Home Connect account has no dishwasher{}",
            if others.is_empty() {
                String::new()
            } else {
                format!(", only {}", others.join(", "))
            }
        );
    }
    Ok(dishwashers.into_iter().cloned().collect())
}

/// A dishwasher by its type, or a model number that only dishwashers have.
fn is_dishwasher(item: &Found) -> bool {
    let kind = item.kind.to_ascii_lowercase();
    let model = item.model.to_ascii_uppercase();
    kind.contains("dish")
        || model.starts_with("SPV")
        || model.starts_with("SMS")
        || model.starts_with("SMV")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(id: &str, kind: &str, model: &str) -> Found {
        Found {
            ha_id: id.into(),
            brand: "BOSCH".into(),
            kind: kind.into(),
            model: model.into(),
        }
    }

    #[test]
    fn a_dishwasher_is_known_by_its_type_or_its_model() {
        assert!(is_dishwasher(&found("A", "Dishwasher", "")));
        assert!(is_dishwasher(&found("B", "", "SMS46IW01G")));
        assert!(!is_dishwasher(&found("C", "Oven", "HBA5780S0")));
    }
}
