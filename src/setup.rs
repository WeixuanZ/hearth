//! Adding a device, as steps any surface can show.
//!
//! A connector describes each step of its setup: what it says, what it asks,
//! a page to open, or that it is waiting on the device. The menu bar draws a
//! step as a form and the CLI asks it in the terminal, so neither knows a
//! brand, and each flow is written once. A step that fails can be answered
//! again: a flow moves on only when a step succeeds.

use crate::{capabilities::Item, config};
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::Write, sync::Mutex};

pub type Answers = BTreeMap<String, String>;

/// What to show, and what to ask.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub title: String,
    pub note: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    /// A page to open, like a sign-in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// The field the page fills by itself, where a surface can: it sends the
    /// browser back to an address the menu bar app answers. Typed otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback: Option<&'static str>,
    /// Waiting on the device, like Allow on a TV: continue with no answers.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub wait: bool,
}

impl Step {
    pub fn new(title: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            note: note.into(),
            fields: Vec::new(),
            link: None,
            callback: None,
            wait: false,
        }
    }

    pub fn field(mut self, field: Field) -> Self {
        self.fields.push(field);
        self
    }

    pub fn link(mut self, link: String) -> Self {
        self.link = Some(link);
        self
    }

    pub fn callback(mut self, field: &'static str) -> Self {
        self.callback = Some(field);
        self
    }

    pub fn waiting(mut self) -> Self {
        self.wait = true;
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Field {
    pub id: &'static str,
    pub label: String,
    #[serde(flatten)]
    pub kind: FieldKind,
    /// Filled in already, and kept if left alone.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub value: String,
    /// Shown while empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hint: String,
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Secret,
    Choice { options: Vec<Item> },
}

impl Field {
    fn new(id: &'static str, label: &str, kind: FieldKind) -> Self {
        Self {
            id,
            label: label.into(),
            kind,
            value: String::new(),
            hint: String::new(),
            optional: false,
        }
    }

    pub fn text(id: &'static str, label: &str) -> Self {
        Self::new(id, label, FieldKind::Text)
    }

    pub fn secret(id: &'static str, label: &str) -> Self {
        Self::new(id, label, FieldKind::Secret)
    }

    pub fn choice(id: &'static str, label: &str, options: Vec<Item>) -> Self {
        Self::new(id, label, FieldKind::Choice { options })
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

pub enum Next {
    Ask(Step),
    /// The device is saved. What to tell the person.
    Done(String),
}

/// An answer that must be there.
pub fn answer(answers: &Answers, id: &str) -> Result<String> {
    given(answers, id).ok_or_else(|| anyhow!("{id} is needed"))
}

/// An answer, if one was given.
pub fn given(answers: &Answers, id: &str) -> Option<String> {
    answers
        .get(id)
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// A device that can be added, for the menu bar's list.
#[derive(Debug, Clone, Serialize)]
pub struct Product {
    pub id: &'static str,
    pub name: &'static str,
    /// The kind of device, which picks its icon.
    pub category: &'static str,
}

/// The devices that can be added, from the drivers.
pub fn products() -> Vec<Product> {
    crate::driver::all()
        .into_iter()
        .map(|driver| driver.product())
        .collect()
}

/// A setup for `product`, and its first step.
fn start(product: &str) -> Result<(Box<dyn crate::driver::AnySetup>, Step)> {
    crate::driver::find(product)
        .map(|driver| driver.setup())
        .ok_or_else(|| anyhow!("no device called {product} can be added. These can: {}", kinds()))
}

/// Setups under way for the control socket, one per product.
static FLOWS: Mutex<BTreeMap<String, Box<dyn crate::driver::AnySetup>>> =
    Mutex::new(BTreeMap::new());

/// One step of adding `product`, for the control socket. No answers starts
/// it over; answers continue it, to `{"step": …}` or `{"done": "…"}`.
pub async fn step(product: &str, answers: Option<Answers>) -> Result<Value> {
    let flows = || FLOWS.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(answers) = answers else {
        let (flow, step) = start(product)?;
        flows().insert(product.to_owned(), flow);
        return Ok(json!({ "step": step }));
    };
    let mut flow = flows()
        .remove(product)
        .ok_or_else(|| anyhow!("adding {product} was not started, or has finished"))?;
    match flow.next(&answers).await {
        Ok(Next::Ask(step)) => {
            flows().insert(product.to_owned(), flow);
            Ok(json!({ "step": step }))
        }
        Ok(Next::Done(message)) => Ok(json!({ "done": message })),
        Err(error) => {
            // Same step again, to correct an answer.
            flows().insert(product.to_owned(), flow);
            Err(error)
        }
    }
}

/// Runs a setup in the terminal. `given` answers questions first, as from
/// the command's flags; the rest are asked.
/// The kinds that can be added, as `hearth add` takes them.
fn kinds() -> String {
    let all: Vec<String> = products()
        .into_iter()
        .map(|product| format!("{} ({})", product.id, product.name))
        .collect();
    all.join(", ")
}

pub async fn in_terminal(product: &str, given: Answers) -> Result<()> {
    if product == "list" {
        println!("{}", kinds().replace(", ", "\n"));
        return Ok(());
    }
    let (mut flow, mut step) = start(product)?;
    loop {
        println!("{}", step.title);
        if !step.note.is_empty() {
            println!("{}", step.note);
        }
        if let Some(link) = &step.link {
            println!();
            println!("{link}");
            println!();
        }
        let mut answers = Answers::new();
        for field in &step.fields {
            let value = match given.get(field.id) {
                Some(value) => value.clone(),
                None => ask(field)?,
            };
            answers.insert(field.id.to_owned(), value);
        }
        match flow.next(&answers).await? {
            Next::Ask(next) => step = next,
            Next::Done(message) => {
                println!("{message}");
                println!("Apple Home pairing code: {}", config::load()?.homekit_pin);
                return Ok(());
            }
        }
        println!();
    }
}

fn ask(field: &Field) -> Result<String> {
    if let FieldKind::Choice { options } = &field.kind {
        for (index, option) in options.iter().enumerate() {
            println!("  {}. {}", index + 1, option.name);
        }
    }
    let label = match (&field.kind, field.value.is_empty()) {
        (FieldKind::Choice { .. }, _) => format!("{} (number): ", field.label),
        (_, false) => format!("{} [{}]: ", field.label, field.value),
        (_, true) if field.optional => format!("{} (optional): ", field.label),
        _ => format!("{}: ", field.label),
    };
    let typed = if matches!(field.kind, FieldKind::Secret) {
        rpassword::prompt_password(label)?
    } else {
        print!("{label}");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line.trim().to_owned()
    };
    Ok(match &field.kind {
        FieldKind::Choice { options } => typed
            .parse::<usize>()
            .ok()
            .and_then(|number| options.get(number.wrapping_sub(1)))
            .map(|option| option.id.clone())
            .unwrap_or(typed),
        _ if typed.is_empty() => field.value.clone(),
        _ => typed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_is_a_form_any_surface_can_draw() {
        let step = Step::new("Sign in", "Once.")
            .field(Field::text("email", "Email").hint("you@example.com"))
            .field(Field::secret("password", "Password"))
            .field(Field::choice("fan", "Fan", vec![Item::new("a", "Study fan")]).optional())
            .link("https://example.com".into());
        let json = serde_json::to_value(&step).unwrap();
        assert_eq!(json["fields"][0]["kind"], "text");
        assert_eq!(json["fields"][0]["hint"], "you@example.com");
        assert_eq!(json["fields"][1]["kind"], "secret");
        assert_eq!(json["fields"][2]["options"][0]["name"], "Study fan");
        assert_eq!(json["fields"][2]["optional"], true);
        assert_eq!(json["link"], "https://example.com");
        assert!(json.get("wait").is_none());
        assert!(serde_json::to_value(Step::new("Wait", "").waiting()).unwrap()["wait"] == true);
    }

    #[tokio::test]
    async fn the_socket_starts_a_setup_and_refuses_one_never_started() {
        let started = step("bosch", None).await.unwrap();
        assert!(started["step"]["link"]
            .as_str()
            .unwrap()
            .starts_with("https://"));
        assert_eq!(started["step"]["callback"], "redirect");
        assert!(step("toaster", None).await.is_err());
        let error = step("frame", Some(Answers::new())).await.unwrap_err();
        assert!(error.to_string().contains("not started"));
        assert_eq!(
            given(&Answers::from([("a".into(), "  ".into())]), "a"),
            None
        );
    }
}
