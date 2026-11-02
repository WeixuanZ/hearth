//! The dishwasher as capabilities, for the menu bar and the CLI.
//! HomeKit shows the same state through `accessory`, and both send the same commands.

use crate::connectors::bosch::protocol::{
    feature_uid, format_remaining, label_of, option_switches, programs, Command, Machine, Reading,
};
use crate::{
    capabilities::{About, Alert, Capability, Item, Role, Set, Snapshot},
    config::Settings as _,
    devices::dishwasher::bosch::BoschDevice,
};
use anyhow::{anyhow, Result};

/// What the bridge keeps beside the machine's own state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Kept {
    /// The programme the next start runs. Choosing one sends nothing.
    pub chosen: Option<u32>,
    /// The last run ended without being stopped (see `devices::cycle`).
    pub finished: bool,
}

/// What a request comes to.
#[derive(Debug, PartialEq)]
pub enum Ask {
    Send(Command),
    /// The programme for the next start, kept by the bridge.
    Choose(u32),
}

/// The programme and its options, then Start, then what can change while it
/// runs, then what it reports. While a programme runs its settings are
/// fixed, so they are inactive.
pub fn snapshot(device: &BoschDevice, reading: &Reading, kept: Kept) -> Snapshot {
    let machine = &reading.machine;
    let running = machine.running();
    let chosen = chosen(device, machine, kept);
    let mut capabilities = Vec::new();
    let programmes = programs(&device.features);
    if !programmes.is_empty() {
        capabilities.push(
            Capability::choice(
                "programme",
                "Programme",
                programmes
                    .iter()
                    .map(|(uid, name)| Item::new(uid.to_string(), label_of(name)))
                    .collect(),
                chosen.map(|uid| uid.to_string()),
            )
            .role(Role::Programme)
            .active(!running),
        );
    }
    let (during, before): (Vec<_>, Vec<_>) = option_switches(&device.features)
        .into_iter()
        .partition(|(key, _, _)| during_run(key));
    let switch = |(key, _, label): (&str, u64, &str), active: bool| {
        let on = machine.options.get(key).copied().unwrap_or(false);
        Capability::switch(option_id(key), label, on).active(active)
    };
    for option in before {
        capabilities.push(switch(option, machine.remote_start && !running));
    }
    capabilities.push(
        Capability::run(
            running,
            blocked(device, machine, chosen),
            u32::try_from(machine.remaining_secs)
                .ok()
                .filter(|secs| *secs > 0),
            Some(machine.progress as f32),
        )
        .finished(kept.finished),
    );
    for option in during {
        capabilities.push(switch(option, running));
    }
    capabilities.extend(readings(machine));
    // Known once the machine has said, which a connection that just opened
    // has not.
    let known = |value: bool| (reading.online && !machine.operation.is_empty()).then_some(value);
    capabilities
        .push(Capability::state("door", "Door open", known(machine.door_open)).role(Role::Contact));
    capabilities.push(Capability::state("leak", "Leak", known(machine.leak)).role(Role::Leak));
    // Only what this machine reports: not every one tracks its rinse aid.
    let reports =
        |part: fn(&str) -> bool| device.features.values().any(|feature| part(&feature.name));
    if reports(crate::connectors::bosch::protocol::salt_name) {
        capabilities
            .push(Capability::state("salt", "Salt low", machine.salt_low).role(Role::Supply));
    }
    if reports(|name| name.contains("RinseAid")) {
        capabilities.push(
            Capability::state("rinse_aid", "Rinse aid low", machine.rinse_low).role(Role::Supply),
        );
    }

    let programme = if machine.program.is_empty() {
        String::new()
    } else {
        label_of(&machine.program)
    };
    let mut detail = if machine.remaining_secs == 0 || !running {
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
        headline: headline(reading, kept),
        detail,
        capabilities,
        alerts: if reading.online {
            alerts(machine, kept)
        } else {
            Vec::new()
        },
        about: About {
            maker: if device.brand.is_empty() {
                "Bosch".into()
            } else {
                device.brand.clone()
            },
            model: if device.model.is_empty() {
                "Dishwasher".into()
            } else {
                device.model.clone()
            },
            serial: if device.ha_id.chars().count() > 1 {
                device.ha_id.clone()
            } else {
                "dishwasher".into()
            },
            firmware: env!("CARGO_PKG_VERSION").into(),
        },
    }
}

/// The chosen programme, else the one the machine has selected.
fn chosen(device: &BoschDevice, machine: &Machine, kept: Kept) -> Option<u32> {
    kept.chosen.or_else(|| {
        programs(&device.features)
            .into_iter()
            .find_map(|(uid, name)| selected(&machine.program, &name).then_some(uid))
    })
}

/// Why it cannot start, or stop, from here now.
fn blocked(device: &BoschDevice, machine: &Machine, chosen: Option<u32>) -> Option<String> {
    let why = if machine.running() {
        feature_uid(&device.features, "AbortProgram")
            .is_none()
            .then_some("This dishwasher stops only from its own buttons")
    } else if !machine.remote_start {
        Some("Press Remote Start on the dishwasher to start it from here")
    } else {
        chosen.is_none().then_some("Choose a programme to start")
    };
    why.map(str::to_owned)
}

fn alerts(machine: &Machine, kept: Kept) -> Vec<Alert> {
    [
        (kept.finished, "finished", "The programme has finished."),
        (
            machine.salt_low == Some(true),
            "salt",
            "The salt is low. Refill it.",
        ),
        (
            machine.rinse_low == Some(true),
            "rinse_aid",
            "The rinse aid is low. Refill it.",
        ),
    ]
    .into_iter()
    .filter(|(holds, ..)| *holds)
    .map(|(_, id, message)| Alert::new(id, message))
    .collect()
}

fn headline(reading: &Reading, kept: Kept) -> String {
    if !reading.online {
        "Looking".into()
    } else if reading.machine.running() {
        "Running".into()
    } else if kept.finished {
        "Finished".into()
    } else if reading.machine.power {
        "On".into()
    } else {
        "Off".into()
    }
}
fn readings(machine: &crate::connectors::bosch::protocol::Machine) -> Vec<Capability> {
    let mut out = Vec::new();
    // The machine's forecast for the programme chosen, as a share of the most
    // any of its programmes uses: 51% is half its hungriest. Not an amount.
    if let Some(value) = number_in(&machine.energy) {
        out.push(Capability::reading(
            "energy",
            "Energy (of max)",
            "%",
            Some(value),
        ));
    }
    if let Some(value) = number_in(&machine.water) {
        out.push(Capability::reading(
            "water",
            "Water (of max)",
            "%",
            Some(value),
        ));
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

/// An option for a programme already running, like silence for half an
/// hour. Every other option is set before the start.
fn during_run(key: &str) -> bool {
    key == "SilenceOnDemand"
}

pub(super) fn option_id(key: &str) -> &'static str {
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

/// What a request comes to, refused while the dishwasher would ignore it.
/// Nothing is kept for later: a programme asked for an hour ago must not
/// start now. Starting needs Remote Start, pressed on the machine.
pub fn command(device: &BoschDevice, reading: &Reading, kept: Kept, set: &Set) -> Result<Ask> {
    let ask = to_ask(device, &reading.machine, kept, set)?;
    let Ask::Send(command) = &ask else {
        return Ok(ask);
    };
    if !reading.online {
        anyhow::bail!("the dishwasher is not connected, so nothing was sent");
    }
    let starts = !matches!(command, Command::Abort);
    if starts && !reading.machine.remote_start {
        anyhow::bail!(
            "the dishwasher is not accepting remote start. Press Remote Start on it first"
        );
    }
    Ok(ask)
}

fn to_ask(device: &BoschDevice, machine: &Machine, kept: Kept, set: &Set) -> Result<Ask> {
    let running = machine.running();
    Ok(match set.capability.as_str() {
        "run" if set.bool()? => {
            if running {
                anyhow::bail!("a programme is already running");
            }
            let uid =
                chosen(device, machine, kept).ok_or_else(|| anyhow!("choose a programme first"))?;
            Ask::Send(Command::Program(uid))
        }
        "run" => {
            if !running {
                anyhow::bail!("no programme is running");
            }
            if let Some(why) = blocked(device, machine, None) {
                anyhow::bail!("{why}");
            }
            Ask::Send(Command::Abort)
        }
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
            Ask::Choose(uid)
        }
        other => {
            let key = option_switches(&device.features)
                .into_iter()
                .find(|(key, _, _)| option_id(key) == other)
                .map(|(key, _, _)| key)
                .ok_or_else(|| set.unknown())?;
            if running != during_run(key) {
                anyhow::bail!(if running {
                    "options are fixed while a programme runs"
                } else {
                    "that option is for a programme that is running"
                });
            }
            Ask::Send(Command::Option {
                key: key.to_owned(),
                on: set.bool()?,
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::bosch::protocol::Machine;

    fn device() -> BoschDevice {
        use crate::connectors::bosch::protocol::BoschFeature;
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
    fn a_finished_programme_and_low_salt_are_alerts_while_it_is_connected() {
        let kept = Kept {
            finished: true,
            ..Kept::default()
        };
        let machine = Machine {
            salt_low: Some(true),
            rinse_low: Some(false),
            ..Machine::default()
        };
        let alerts = |online| {
            let reading = Reading {
                online,
                machine: machine.clone(),
            };
            let ids: Vec<_> = snapshot(&device(), &reading, kept)
                .alerts
                .iter()
                .map(|alert| alert.id)
                .collect();
            ids
        };
        assert_eq!(alerts(true), ["finished", "salt"]);
        assert_eq!(alerts(false), Vec::<&str>::new());
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
            Kept::default(),
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
        // Running, the settings are fixed. This machine offers no remote
        // stop, so Stop is not offered either; the run shows the time left.
        let shown: Vec<_> = snap
            .capabilities
            .iter()
            .filter(|capability| capability.active)
            .map(|capability| capability.id)
            .collect();
        // What it reports stays: its door and leak.
        assert_eq!(shown, ["door", "leak"]);
        let run = serde_json::to_value(snap.capabilities.iter().find(|c| c.id == "run")).unwrap();
        assert_eq!(run["value"], true);
        assert_eq!(run["remaining"], 5400);
    }

    fn refused(result: Result<Ask>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn choosing_sends_nothing_and_start_sends_the_choice() {
        let device = device();
        let mut reading = Reading {
            online: true,
            ..Reading::default()
        };
        let choose = Set::test("programme", serde_json::json!("Eco 50"));
        let nowhere = Reading::default();
        assert_eq!(
            command(&device, &nowhere, Kept::default(), &choose).unwrap(),
            Ask::Choose(30)
        );

        let start = Set::test("run", serde_json::json!(true));
        let none = Kept::default();
        assert!(refused(command(&device, &reading, none, &start)).contains("choose"));
        let chosen = Kept {
            chosen: Some(30),
            ..none
        };
        assert!(refused(command(&device, &nowhere, chosen, &start)).contains("not connected"));
        assert!(refused(command(&device, &reading, chosen, &start)).contains("Remote Start"));
        reading.machine.remote_start = true;
        assert_eq!(
            command(&device, &reading, chosen, &start).unwrap(),
            Ask::Send(Command::Program(30))
        );
    }

    #[test]
    fn a_running_programme_stops_only_where_the_machine_offers_it() {
        use crate::connectors::bosch::protocol::BoschFeature;
        let mut device = device();
        let reading = Reading {
            online: true,
            machine: Machine {
                operation: "BSH.Common.EnumType.OperationState.Run".into(),
                ..Machine::default()
            },
        };
        let stop = Set::test("run", serde_json::json!(false));
        let kept = Kept::default();
        assert!(refused(command(&device, &reading, kept, &stop)).contains("own buttons"));
        device.features.insert(
            "50".into(),
            BoschFeature {
                name: "BSH.Common.Command.AbortProgram".into(),
                values: Default::default(),
            },
        );
        assert_eq!(
            command(&device, &reading, kept, &stop).unwrap(),
            Ask::Send(Command::Abort)
        );
        let option = Set::test("extra_dry", serde_json::json!(true));
        assert!(refused(command(&device, &reading, kept, &option)).contains("fixed"));
    }
}
