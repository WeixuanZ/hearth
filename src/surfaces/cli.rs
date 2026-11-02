//! What the CLI prints: tables for people, JSON for scripts and agents.

use crate::{config, series, surfaces::control};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Local};
use serde_json::{json, Value};
use std::io::IsTerminal;

/// A typed value from the command line: on/off, a number, or an option id.
pub fn parse_value(text: &str) -> Value {
    match text.to_lowercase().as_str() {
        "on" | "true" | "yes" | "start" => json!(true),
        "off" | "false" | "no" | "stop" => json!(false),
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

/// Readings over the last `hours`. Talks to the bridge when it is running, so
/// any computer in the group sees the same series, and reads the local CSV
/// when it is not.
pub async fn history(
    device: &str,
    capability: Option<&str>,
    hours: u64,
    as_json: bool,
) -> Result<()> {
    let value = if control::running().await {
        control::request(json!({
            "op": "history",
            "device": device,
            "capability": capability,
            "hours": hours,
        }))
        .await?
    } else {
        series::offline_document(device, capability, hours)?
    };
    if as_json {
        print_json(&value)
    } else {
        print_history(&value);
        Ok(())
    }
}

/// A reading's usual day, from the last thirty: each hour's range and mean,
/// and today's value beside it. Asks the bridge, which holds the record.
pub async fn usual(device: &str, capability: &str, as_json: bool) -> Result<()> {
    let value = control::request(json!({
        "op": "normals",
        "device": device,
        "capability": capability,
    }))
    .await?;
    if as_json {
        return print_json(&value);
    }
    let style = Style::detect();
    let unit = value["unit"].as_str().unwrap_or("");
    let days = value["days"].as_u64().unwrap_or(0);
    let normals = value["normals"].as_array().cloned().unwrap_or_default();
    if normals.is_empty() {
        println!("A usual day needs three days of readings; there are {days}.");
        return Ok(());
    }
    // Today's last value in each hour, on the local clock.
    let mut today = std::collections::BTreeMap::new();
    for sample in value["today"].as_array().into_iter().flatten() {
        let (Some(time), Some(reading)) = (sample["t"].as_i64(), sample["v"].as_f64()) else {
            continue;
        };
        if let Some(moment) = DateTime::from_timestamp(time, 0) {
            today.insert(
                moment.with_timezone(&Local).format("%H").to_string(),
                reading,
            );
        }
    }
    let text = |number: f64| {
        format!("{} {unit}", number_text(number))
            .trim_end()
            .to_owned()
    };
    println!(
        "{}",
        style.bold(&format!("Usual {capability}, from the last {days} days"))
    );
    println!(
        "{}",
        style.dim("hour   usual range          mean        today")
    );
    for hour in normals {
        let at = format!("{:02}", hour["hour"].as_u64().unwrap_or(0));
        let range = format!(
            "{} to {}",
            text(hour["low"].as_f64().unwrap_or(0.0)),
            text(hour["high"].as_f64().unwrap_or(0.0))
        );
        let mean = text(hour["mean"].as_f64().unwrap_or(0.0));
        let now = today
            .get(&at)
            .map(|reading| text(*reading))
            .unwrap_or_default();
        println!("{at}     {range:<20} {mean:<11} {now}");
    }
    Ok(())
}

/// Events as they happen, until the bridge stops or this is interrupted:
/// each change, and alerts as they are raised and cleared.
pub async fn events(topic: &str, as_json: bool) -> Result<()> {
    let mut events = control::Subscription::open(topic).await?;
    let style = Style::detect();
    let mut names = std::collections::HashMap::new();
    while let Some(event) = events.next().await? {
        if as_json {
            println!("{event}");
            continue;
        }
        let device = event["device"].as_str().unwrap_or_default().to_owned();
        let time = Local::now().format("%H:%M:%S").to_string();
        let name = |names: &std::collections::HashMap<String, String>| {
            names
                .get(&device)
                .cloned()
                .unwrap_or_else(|| device.clone())
        };
        match event["event"].as_str() {
            // A device's whole state; remembered for its name.
            Some("device") => {
                let snapshot = &event["device"];
                if let (Some(id), Some(name)) = (snapshot["id"].as_str(), snapshot["name"].as_str())
                {
                    names.insert(id.to_owned(), name.to_owned());
                }
            }
            Some("changed") => println!(
                "{}  {}  {} {}",
                style.dim(&time),
                style.bold(&name(&names)),
                event["capability"].as_str().unwrap_or_default(),
                event["value"]
            ),
            Some("raised") => println!(
                "{}  {}  {} {}",
                style.dim(&time),
                style.bold(&name(&names)),
                style.wrap("33", "!"),
                event["alert"]["message"].as_str().unwrap_or_default()
            ),
            Some("cleared") => println!(
                "{}  {}  {}",
                style.dim(&time),
                style.bold(&name(&names)),
                style.dim(&format!(
                    "{} cleared",
                    event["alert"].as_str().unwrap_or_default()
                ))
            ),
            _ => {}
        }
    }
    Ok(())
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

/// Sends a group op, if any, then shows the group.
pub async fn group(op: Option<Value>, as_json: bool) -> Result<()> {
    if let Some(op) = op {
        control::request(op).await?;
    }
    let status = control::request(json!({ "op": "status" })).await?;
    let group = &status["group"];
    if as_json {
        return print_json(group);
    }
    let text = |key: &str| group[key].as_str().unwrap_or_default().to_owned();
    let style = Style::detect();
    let doing = match text("role").as_str() {
        "serving" => "serves Home".to_owned(),
        "conflict" => "serves Home, and another member serves a different Home".to_owned(),
        "standby" => format!("is on standby; {} serves Home", text("leader")),
        "joining" => format!("is asking {} to let it join", text("leader")),
        _ => "is looking for a group".to_owned(),
    };
    println!("{} {doing}.", style.bold(&text("name")));
    let members: Vec<&str> = group["members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !members.is_empty() {
        println!("With {}.", members.join(", "));
    }
    if !text("code").is_empty() {
        println!(
            "Type {} on {}, then run `hearth group confirm` here.",
            style.bold(&text("code")),
            text("leader")
        );
    }
    if !text("asking").is_empty() {
        println!(
            "{} asks to join. Run `hearth group approve <the code it shows>`, or `hearth group decline`.",
            text("asking")
        );
    }
    if !text("other").is_empty() {
        println!(
            "{} is in another group. `hearth group join` leaves this one for it.",
            text("other")
        );
    }
    Ok(())
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_history(value: &Value) {
    let style = Style::detect();
    let device = value["name"].as_str().unwrap_or("device");
    let hours = value["hours"].as_u64().unwrap_or(24);
    // A value holds until the next sample, unless that is further away than
    // the heartbeat: then the reading was not being measured.
    let gap = value["heartbeat"]
        .as_u64()
        .unwrap_or(series::HEARTBEAT_SECS)
        + value["interval"].as_u64().unwrap_or(series::INTERVAL_SECS);
    let path = value["path"].as_str().unwrap_or("");
    let series = value["series"].as_array();
    let Some(series) = series else {
        println!("{device} has no readings.");
        return;
    };
    if series.is_empty() {
        println!("{device} has no readings.");
        return;
    }
    for (index, reading) in series.iter().enumerate() {
        if index > 0 {
            println!();
        }
        print_reading(device, reading, hours, gap, &style);
    }
    if !path.is_empty() {
        println!();
        println!("{}", style.dim(path));
    }
}

fn print_reading(device: &str, reading: &Value, hours: u64, gap: u64, style: &Style) {
    let name = reading["name"].as_str().unwrap_or("reading");
    let unit = reading["unit"].as_str().unwrap_or("");
    let samples: Vec<(u64, f64)> = reading["samples"]
        .as_array()
        .map(|samples| {
            samples
                .iter()
                .filter_map(|sample| Some((sample["t"].as_u64()?, sample["v"].as_f64()?)))
                .collect()
        })
        .unwrap_or_default();
    let span = if hours == 1 {
        "last hour".to_owned()
    } else {
        format!("last {hours} hours")
    };
    println!(
        "{}  {}",
        style.bold(&format!("{device} · {name}")),
        style.dim(&span)
    );
    if samples.is_empty() {
        println!("No readings in the {span}.");
        return;
    }
    let latest = samples.last().map(|(_, value)| *value).unwrap_or(0.0);
    let min = samples
        .iter()
        .map(|(_, value)| *value)
        .fold(f64::INFINITY, f64::min);
    let max = samples
        .iter()
        .map(|(_, value)| *value)
        .fold(f64::NEG_INFINITY, f64::max);
    println!(
        "{} now    {} min    {} max    {} {}",
        with_unit(latest, unit),
        with_unit(min, unit),
        with_unit(max, unit),
        samples.len(),
        if samples.len() == 1 {
            "sample"
        } else {
            "samples"
        }
    );
    let line = sparkline(&samples, gap);
    println!("{line}");
    if let (Some((start, _)), Some((end, _))) = (samples.first(), samples.last()) {
        let start = clock(*start, hours);
        let end = clock(*end, hours);
        let gap = line
            .chars()
            .count()
            .saturating_sub(start.chars().count() + end.chars().count());
        println!("{}", style.dim(&format!("{start}{}{end}", " ".repeat(gap))));
    }
}

fn with_unit(value: f64, unit: &str) -> String {
    let text = series::milli_of(value).map_or_else(|| format!("{value}"), series::format_value);
    if unit.is_empty() {
        text
    } else {
        format!("{text} {unit}")
    }
}

fn clock(unix: u64, hours: u64) -> String {
    let Some(time) = DateTime::from_timestamp(unix as i64, 0) else {
        return String::new();
    };
    let pattern = if hours > 48 { "%m-%d %H:%M" } else { "%H:%M" };
    time.with_timezone(&Local).format(pattern).to_string()
}

/// 48 columns across the whole span. A column with no sample of its own
/// holds the last value until `gap` seconds have passed; after that it is
/// blank, a gap rather than a zero.
fn sparkline(samples: &[(u64, f64)], gap: u64) -> String {
    const WIDTH: usize = 48;
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let Some((start, _)) = samples.first() else {
        return String::new();
    };
    let Some((end, _)) = samples.last() else {
        return String::new();
    };
    if samples.len() == 1 || start == end {
        return "▄".to_owned();
    }
    let span = end.saturating_sub(*start).max(1);
    let mut bins: Vec<Vec<f64>> = vec![Vec::new(); WIDTH];
    for (time, value) in samples {
        let index = ((*time - start) as u128 * (WIDTH as u128 - 1) / span as u128) as usize;
        bins[index.min(WIDTH - 1)].push(*value);
    }
    let averages: Vec<Option<f64>> = bins
        .iter()
        .enumerate()
        .map(|(index, bin)| {
            if !bin.is_empty() {
                return Some(bin.iter().sum::<f64>() / bin.len() as f64);
            }
            let at = start + (index as u128 * span as u128 / (WIDTH as u128 - 1)) as u64;
            let held = samples.partition_point(|(time, _)| *time <= at);
            let (time, value) = samples[held.checked_sub(1)?];
            (at - time <= gap).then_some(value)
        })
        .collect();
    let known: Vec<f64> = averages.iter().copied().flatten().collect();
    let min = known.iter().copied().fold(f64::INFINITY, f64::min);
    let max = known.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;
    averages
        .into_iter()
        .map(|value| match value {
            None => ' ',
            Some(_) if range < f64::EPSILON => '▄',
            Some(value) => {
                let scaled = ((value - min) / range * (BLOCKS.len() - 1) as f64).round() as usize;
                BLOCKS[scaled.min(BLOCKS.len() - 1)]
            }
        })
        .collect()
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
    for alert in device["alerts"].as_array().into_iter().flatten() {
        let message = alert["message"].as_str().unwrap_or_default();
        println!("{} {message}", style.wrap("33", "!"));
    }
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
        Some("state") => if value == true { "yes" } else { "no" }.into(),
        Some("run") if value == true => match capability["remaining"].as_u64() {
            Some(secs) => match (secs / 3600, secs % 3600 / 60) {
                (0, minutes) => format!("running, {minutes} min left"),
                (hours, minutes) => format!("running, {hours} h {minutes} min left"),
            },
            None => "running".into(),
        },
        Some("run") => "stopped".into(),
        Some("choice") => {
            let option = capability["options"]
                .as_array()
                .and_then(|options| options.iter().find(|option| option["id"] == *value));
            let name = option
                .and_then(|option| option["name"].as_str())
                .unwrap_or_else(|| value.as_str().unwrap_or(""));
            match option.and_then(|option| option["note"].as_str()) {
                Some(note) => format!("{name} ({})", note.to_lowercase()),
                None => name.into(),
            }
        }
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
    // Opaque ids, like a dishwasher's programme numbers, are listed by name,
    // which `set` takes as well.
    let options = || {
        let items = capability["options"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let opaque = items.iter().any(|item| {
            item["id"]
                .as_str()
                .is_some_and(|id| id.chars().all(|c| c.is_ascii_digit()))
        });
        if !opaque {
            return ids("options");
        }
        items
            .iter()
            .filter_map(|item| item["name"].as_str())
            .map(|name| {
                if name.contains(' ') {
                    format!("\"{name}\"")
                } else {
                    name.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };
    match capability["kind"].as_str() {
        Some("power" | "switch") => "on | off".into(),
        Some("run") => match capability["blocked"].as_str() {
            Some(why) => why.to_owned(),
            None => "start | stop".into(),
        },
        Some("choice") => options(),
        Some("keys") => format!("press: {}", ids("keys")),
        Some("text") => "words, in quotes".into(),
        Some("level") => {
            let number = |key: &str| number_text(capability[key].as_f64().unwrap_or(0.0));
            format!(
                "{}–{}, step {}",
                number("min"),
                number("max"),
                number("step")
            )
        }
        Some("reading" | "state") => "read only".into(),
        _ => String::new(),
    }
}

/// Bold and dim, only on a terminal, and never with NO_COLOR set.
pub(super) struct Style {
    color: bool,
}

impl Style {
    pub(super) fn detect() -> Self {
        Self {
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    pub(super) fn bold(&self, text: &str) -> String {
        self.wrap("1", text)
    }

    pub(super) fn dim(&self, text: &str) -> String {
        self.wrap("2", text)
    }

    pub(super) fn wrap(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }
}

#[cfg(test)]
mod sparkline_tests {
    use super::sparkline;

    #[test]
    fn a_steady_value_holds_and_only_a_long_silence_is_a_gap() {
        // Stored on change and every 15 minutes: steady, so one line.
        let steady: Vec<(u64, f64)> = (0..=8).map(|n| (n * 900, 66.0)).collect();
        assert!(!sparkline(&steady, 960).contains(' '));
        // Off for two hours in the middle.
        let gapped = [(0, 20.0), (900, 20.0), (8100, 21.0), (9000, 21.0)];
        let line = sparkline(&gapped, 960);
        assert!(line.contains(' '));
        assert!(!line.starts_with(' ') && !line.ends_with(' '));
    }
}
