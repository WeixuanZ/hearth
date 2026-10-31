//! The dishwasher as capabilities, for the menu bar and the CLI.
//! HomeKit shows the same state through `accessory`, and both send the same commands.

use crate::connectors::bosch::protocol::{
    format_remaining, label_of, option_switches, programs, Command, Reading,
};
use crate::{
    capabilities::{Capability, Item, Set, Snapshot},
    config::{BoschDevice, Settings as _},
};
use anyhow::{anyhow, Result};

pub fn snapshot(device: &BoschDevice, reading: &Reading) -> Snapshot {
    let machine = &reading.machine;
    let can_start = machine.remote_start;
    let mut capabilities =
        vec![Capability::power(machine.power).active(machine.power || can_start)];
    let programmes = programs(&device.features);
    let current = programmes
        .iter()
        .find_map(|(uid, name)| selected(&machine.program, name).then(|| uid.to_string()));
    if !programmes.is_empty() {
        capabilities.push(
            Capability::choice(
                "programme",
                "Programme",
                programmes
                    .iter()
                    .map(|(uid, name)| Item::new(uid.to_string(), label_of(name)))
                    .collect(),
                current,
            )
            .active(can_start),
        );
    }
    for (key, _, label) in option_switches(&device.features) {
        let on = machine.options.get(key).copied().unwrap_or(false);
        capabilities.push(Capability::switch(option_id(key), label, on).active(can_start));
    }
    capabilities.extend(readings(machine));

    let programme = if machine.program.is_empty() {
        String::new()
    } else {
        label_of(&machine.program)
    };
    let mut detail = if machine.remaining_secs == 0 {
        programme
    } else if programme.is_empty() {
        format_remaining(machine.remaining_secs)
    } else {
        format!("{programme} · {}", format_remaining(machine.remaining_secs))
    };
    if machine.door_open {
        push_detail(&mut detail, "door open");
    }
    if machine.salt_low == Some(true) {
        push_detail(&mut detail, "salt low");
    }
    if machine.rinse_low == Some(true) {
        push_detail(&mut detail, "rinse aid low");
    }
    if detail.is_empty() {
        detail = device.model.clone();
    }

    Snapshot {
        id: device.id(),
        kind: "bosch",
        category: "dishwasher",
        name: device.name.clone(),
        online: reading.online,
        headline: headline(reading),
        detail,
        capabilities,
    }
}

fn headline(reading: &Reading) -> String {
    if !reading.online {
        "Looking".into()
    } else if reading.machine.finished {
        "Finished".into()
    } else if reading.machine.power {
        "Running".into()
    } else {
        "Off".into()
    }
}

fn readings(machine: &crate::connectors::bosch::protocol::Machine) -> Vec<Capability> {
    let mut out = vec![Capability::reading(
        "remaining",
        "Remaining",
        "min",
        (machine.remaining_secs > 0).then_some(machine.remaining_secs as f32 / 60.0),
    )];
    if machine.power {
        out.push(Capability::reading(
            "progress",
            "Progress",
            "%",
            Some(machine.progress as f32),
        ));
    }
    if let Some(value) = number_in(&machine.energy) {
        out.push(Capability::reading("energy", "Energy", "kWh", Some(value)));
    }
    if let Some(value) = number_in(&machine.water) {
        out.push(Capability::reading("water", "Water", "L", Some(value)));
    }
    out
}

fn number_in(text: &str) -> Option<f32> {
    let token = text.split_whitespace().next()?;
    let value = token.parse::<f32>().ok()?;
    value.is_finite().then_some(value)
}

fn push_detail(detail: &mut String, note: &str) {
    if !detail.is_empty() {
        detail.push_str(" · ");
    }
    detail.push_str(note);
}

fn selected(current: &str, feature_name: &str) -> bool {
    if current.is_empty() {
        return false;
    }
    let short = feature_name.rsplit('.').next().unwrap_or(feature_name);
    current == feature_name
        || current == label_of(feature_name)
        || feature_name.ends_with(current)
        || current.ends_with(short)
}

fn option_id(key: &str) -> &'static str {
    match key {
        "ExtraDry" => "extra_dry",
        "VarioSpeedPlus" => "speed",
        "SilenceOnDemand" => "silence",
        "HygienePlus" => "hygiene",
        "IntensiveZone" => "intensive",
        "ChildLock" => "child_lock",
        "PermanentRemoteStart" => "remote_start",
        _ => "option",
    }
}

/// What a request asks of the dishwasher, in the commands HomeKit sends too.
/// The command for a request, refused while the dishwasher would ignore it:
/// starting anything needs Remote Start, pressed on the machine.
pub fn command(device: &BoschDevice, reading: &Reading, set: &Set) -> Result<Command> {
    let command = to_command(device, set)?;
    let starts = !matches!(command, Command::Power(false));
    if starts && reading.online && !reading.machine.remote_start {
        anyhow::bail!(
            "the dishwasher is not accepting remote start. Press Remote Start on it first"
        );
    }
    Ok(command)
}

fn to_command(device: &BoschDevice, set: &Set) -> Result<Command> {
    Ok(match set.capability.as_str() {
        "power" => Command::Power(set.bool()?),
        "programme" => {
            let id = set.option()?;
            let uid = programs(&device.features)
                .into_iter()
                .find(|(uid, name)| {
                    uid.to_string() == id || label_of(name).eq_ignore_ascii_case(&id)
                })
                .map(|(uid, _)| uid)
                .ok_or_else(|| {
                    anyhow!("programme takes one of the machine's programmes, not {id}")
                })?;
            Command::Program(uid)
        }
        other => {
            let key = option_switches(&device.features)
                .into_iter()
                .find(|(key, _, _)| option_id(key) == other)
                .map(|(key, _, _)| key)
                .ok_or_else(|| set.unknown())?;
            Command::Option {
                key: key.to_owned(),
                on: set.bool()?,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::bosch::protocol::Machine;

    fn device() -> BoschDevice {
        use crate::config::BoschFeature;
        use std::collections::BTreeMap;
        let mut features = BTreeMap::new();
        features.insert(
            "30".into(),
            BoschFeature {
                name: "Dishcare.Dishwasher.Program.Eco50".into(),
                values: BTreeMap::new(),
            },
        );
        features.insert(
            "40".into(),
            BoschFeature {
                name: "Dishcare.Dishwasher.Option.ExtraDry".into(),
                values: BTreeMap::new(),
            },
        );
        BoschDevice {
            aid: 2,
            door_aid: 3,
            leak_aid: 4,
            cycle_aid: 5,
            name: "Bosch Dishwasher".into(),
            brand: "Bosch".into(),
            model: "SPV2HKX42G".into(),
            ha_id: "BOSCH-HA".into(),
            host: "oven".into(),
            key: "secret-key".into(),
            iv: String::new(),
            features,
        }
    }

    #[test]
    fn the_menu_shows_the_cycle_and_not_the_key() {
        let machine = Machine {
            power: true,
            operation: "Run".into(),
            program: "Dishcare.Dishwasher.Program.Eco50".into(),
            remaining_secs: 5400,
            remote_start: true,
            ..Machine::default()
        };
        let snap = snapshot(
            &device(),
            &Reading {
                online: true,
                machine,
            },
        );
        let text = serde_json::to_string(&snap).unwrap();
        assert!(!text.contains("secret-key"));
        assert_eq!(snap.headline, "Running");
        assert_eq!(snap.detail, "Eco 50 · 1 h 30 min");
        assert_eq!(snap.kind, "bosch");
        assert_eq!(snap.category, "dishwasher");
        let programme = snap
            .capabilities
            .iter()
            .find(|capability| capability.id == "programme")
            .unwrap();
        assert_eq!(programme.value, serde_json::json!("30"));
    }

    #[test]
    fn starting_anything_needs_remote_start_but_turning_off_does_not() {
        let device = device();
        let mut reading = Reading {
            online: true,
            ..Reading::default()
        };
        let start = Set::test("programme", serde_json::json!("30"));
        let error = command(&device, &reading, &start).unwrap_err().to_string();
        assert!(error.contains("Remote Start"));
        assert!(command(&device, &reading, &Set::test("power", false.into())).is_ok());
        reading.machine.remote_start = true;
        assert!(matches!(
            command(&device, &reading, &start),
            Ok(Command::Program(30))
        ));
    }
}
