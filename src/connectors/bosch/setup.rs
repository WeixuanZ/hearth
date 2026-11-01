//! Adding a Bosch dishwasher: one Home Connect sign-in, for its local key.

use super::cloud::{Account, SignIn};
use crate::{
    capabilities::Item,
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
                if let [dishwasher] = account.dishwashers.as_slice() {
                    return add(&account, dishwasher.clone(), host).await;
                }
                let options = account
                    .dishwashers
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
                let chosen = account
                    .dishwashers
                    .iter()
                    .find(|item| item.ha_id.eq_ignore_ascii_case(&id))
                    .cloned()
                    .ok_or_else(|| anyhow!("no dishwasher on the account has id {id}"))?;
                add(account, chosen, host.clone()).await
            }
        }
    }
}

async fn add(account: &Account, chosen: super::cloud::Found, host: Option<String>) -> Result<Next> {
    let name = account.add(&chosen, host.as_deref()).await?;
    Ok(Next::Done(format!(
        "Added {name}. Starting a programme needs Remote Start on the dishwasher; reading it does not."
    )))
}
