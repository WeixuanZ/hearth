//! What the CLI prints: tables for people, JSON for scripts and agents.

use crate::{config, surfaces::control};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::IsTerminal;

/// A typed value from the command line: on/off, a number, or an option id.
pub fn parse_value(text: &str) -> Value {
    match text.to_lowercase().as_str() {
        "on" | "true" | "yes" => json!(true),
        "off" | "false" | "no" => json!(false),
        _ => text
            .parse::<f64>()
            .map_or_else(|_| json!(text), |number| json!(number)),
    }
}

pub async fn devices(as_json: bool) -> Result<()> {
    let status = control::request(json!({ "op": "status" })).await?;
    if as_json {
        return print_json(&status);
    }
    let style = Style::detect();
    let devices = status["devices"].as_array().cloned().unwrap_or_default();
    if devices.is_empty() {
        println!("No devices yet. Add one with `hearth dyson login` or `hearth frame pair`.");
    }
    for (index, device) in devices.iter().enumerate() {
        if index > 0 {
            println!();
        }
        print_device(device, &style);
    }
    Ok(())
}

pub async fn get(device: &str, capability: Option<&str>, as_json: bool) -> Result<()> {
    let device = control::request(json!({ "op": "get", "device": device })).await?;
    let Some(id) = capability else {
        return if as_json {
            print_json(&device)
        } else {
            print_device(&device, &Style::detect());
            Ok(())
        };
    };
    let capability = device["capabilities"]
        .as_array()
        .and_then(|all| all.iter().find(|capability| capability["id"] == id))
        .ok_or_else(|| {
            anyhow!(
                "{} has no capability {id}. Run `hearth get {}`",
                device["name"],
                device["id"]
            )
        })?;
    if as_json {
        print_json(capability)
    } else {
        println!("{}", value_text(capability));
        Ok(())
    }
}

pub async fn set(device: &str, capability: &str, value: &str, as_json: bool) -> Result<()> {
    let value = parse_value(value);
    control::request(
        json!({ "op": "set", "device": device, "capability": capability, "value": value }),
    )
    .await?;
    if as_json {
        print_json(&json!({ "ok": true }))
    } else {
        Ok(())
    }
}

/// Renames through the running bridge, which tells Home at once, or in the
/// config when the bridge is off.
pub async fn rename(device: &str, name: &str, as_json: bool) -> Result<()> {
    if control::running().await {
        control::request(json!({ "op": "rename", "device": device, "name": name })).await?;
    } else {
        let mut config = config::load()?;
        config::rename(&mut config, device, name)?;
        config::save(&config)?;
    }
    if as_json {
        print_json(&json!({ "ok": true }))
    } else {
        println!("Renamed {device} to {}.", name.trim());
        Ok(())
    }
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_device(device: &Value, style: &Style) {
    let text = |key: &str| device[key].as_str().unwrap_or("").to_owned();
    let state = if device["online"] == true {
        text("headline")
    } else {
        style.dim(&text("headline"))
    };
    println!("{}  {}", style.bold(&text("name")), state);
    println!(
        "{}",
        style.dim(&format!(
            "{} · {} · {}",
            text("id"),
            text("category"),
            text("detail")
        ))
    );
    let capabilities = device["capabilities"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let width = capabilities
        .iter()
        .filter_map(|capability| capability["id"].as_str())
        .map(str::len)
        .max()
        .unwrap_or(0);
    for capability in &capabilities {
        let id = capability["id"].as_str().unwrap_or("");
        let row = format!(
            "  {id:<width$}  {:<14}  {}",
            value_text(capability),
            accepts(capability)
        );
        if capability["active"] == false {
            println!("{}", style.dim(&row));
        } else {
            println!("{row}");
        }
    }
}

/// A capability's value as a person reads it.
fn value_text(capability: &Value) -> String {
    let value = &capability["value"];
    match capability["kind"].as_str() {
        _ if value.is_null() => "–".into(),
        Some("power" | "switch") => if value == true { "on" } else { "off" }.into(),
        Some("choice") => capability["options"]
            .as_array()
            .and_then(|options| options.iter().find(|option| option["id"] == *value))
            .and_then(|option| option["name"].as_str())
            .unwrap_or_else(|| value.as_str().unwrap_or(""))
            .into(),
        Some("level" | "reading") => {
            let number = number_text(value.as_f64().unwrap_or(0.0));
            let unit = capability["unit"].as_str().unwrap_or("");
            if unit.is_empty() {
                number
            } else {
                format!("{number} {unit}")
            }
        }
        _ => value.to_string(),
    }
}

/// Whole numbers without a point, others to one place.
fn number_text(number: f64) -> String {
    if number.fract() == 0.0 {
        format!("{number:.0}")
    } else {
        format!("{number:.1}")
    }
}

/// What `hearth set` accepts for a capability.
fn accepts(capability: &Value) -> String {
    let ids = |key: &str| {
        capability[key]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["id"].as_str())
                    .collect::<Vec<_>>()
                    .join(" | ")
            })
            .unwrap_or_default()
    };
    match capability["kind"].as_str() {
        Some("power" | "switch") => "on | off".into(),
        Some("choice") => ids("options"),
        Some("keys") => format!("press: {}", ids("keys")),
        Some("level") => {
            let number = |key: &str| number_text(capability[key].as_f64().unwrap_or(0.0));
            format!(
                "{}–{}, step {}",
                number("min"),
                number("max"),
                number("step")
            )
        }
        Some("reading") => "read only".into(),
        _ => String::new(),
    }
}

/// Bold and dim, only on a terminal, and never with NO_COLOR set.
struct Style {
    color: bool,
}

impl Style {
    fn detect() -> Self {
        Self {
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn bold(&self, text: &str) -> String {
        self.wrap("1", text)
    }

    fn dim(&self, text: &str) -> String {
        self.wrap("2", text)
    }

    fn wrap(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }
}
