//! What a dishwasher reports over Home Connect, as hearth keeps it: on, its
//! door, what it is running and how far, its options, salt and rinse aid,
//! leaks, and what its selected programme will use.

use crate::connectors::home_connect::protocol::{feature_uid, Feature, State};
use std::collections::BTreeMap;

pub type Reading = crate::connectors::home_connect::protocol::Reading<Machine>;

/// What Home, and the menu, need from the live machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub power: bool,
    pub operation: String,
    pub program: String,
    pub door_open: bool,
    pub finished: bool,
    pub leak: bool,
    pub salt_low: Option<bool>,
    pub rinse_low: Option<bool>,
    pub remote_start: bool,
    pub remaining_secs: u64,
    pub progress: u64,
    pub energy: String,
    pub water: String,
    pub options: BTreeMap<String, bool>,
}

impl Machine {
    /// Running a programme, or about to: the only time its remaining time
    /// and progress mean anything. Otherwise they are the last programme's.
    /// Being stopped: the end that follows is not a finish.
    pub fn stopping(&self) -> bool {
        self.operation.ends_with("Aborting")
    }

    pub fn running(&self) -> bool {
        self.operation.ends_with("Run") || self.operation.ends_with("DelayedStart")
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self {
            power: false,
            operation: "Inactive".into(),
            program: String::new(),
            door_open: false,
            finished: false,
            leak: false,
            salt_low: None,
            rinse_low: None,
            remote_start: false,
            remaining_secs: 0,
            progress: 0,
            energy: String::new(),
            water: String::new(),
            options: BTreeMap::new(),
        }
    }
}

pub fn apply_value(machine: &mut Machine, name: &str, value: &str) {
    if name.ends_with("PowerState") {
        let value = value.to_ascii_lowercase();
        machine.power = matches!(value.as_str(), "on" | "true" | "1");
    } else if name.ends_with("DoorState") {
        let value = value.to_ascii_lowercase();
        machine.door_open = value.contains("open") || value == "ajar";
    } else if name.ends_with("OperationState") {
        machine.operation = value.to_owned();
    } else if name.ends_with("ProgramFinished") {
        machine.finished = flagged(value);
    } else if name.contains("AquaStop") {
        machine.leak = flagged(value);
    } else if salt_name(name) {
        machine.salt_low = Some(flagged(value));
    } else if name.contains("RinseAid") {
        machine.rinse_low = Some(flagged(value));
    } else if name.ends_with("RemoteControlStartAllowed") {
        machine.remote_start = flagged(value);
    } else if name.ends_with("RemainingProgramTime") && !name.contains("Estimated") {
        machine.remaining_secs = value.parse().unwrap_or(0);
    } else if name.ends_with("ProgramProgress") {
        machine.progress = value.parse().unwrap_or(0);
    } else if name.ends_with("EnergyForecast") {
        machine.energy = value.to_owned();
    } else if name.ends_with("WaterForecast") {
        machine.water = value.to_owned();
    } else if name.ends_with("ActiveProgram") || name.ends_with("SelectedProgram") {
        if value != "0" && !value.is_empty() && !value.eq_ignore_ascii_case("none") {
            machine.program = value.to_owned();
        }
    } else if let Some(key) = option_key(name) {
        machine.options.insert(key.to_owned(), flagged(value));
    }
}

fn flagged(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "on" | "present" | "1" | "low" | "empty" | "locked"
    )
}

pub(crate) fn salt_name(name: &str) -> bool {
    name.contains("Salt") && !name.contains("Program") && !name.contains("Option")
}

/// Option switches, in the order their HomeKit iids are fixed.
const OPTIONS: &[(&str, &str)] = &[
    ("ExtraDry", "Extra Dry"),
    ("VarioSpeedPlus", "SpeedPerfect"),
    ("SilenceOnDemand", "Silence"),
    ("HygienePlus", "Hygiene"),
    ("IntensiveZone", "Intensive zone"),
    ("ChildLock", "Child lock"),
    ("PermanentRemoteStart", "Remote start"),
];

pub fn option_iid(key: &str) -> Option<u64> {
    OPTIONS
        .iter()
        .position(|(name, _)| *name == key)
        .map(|index| 300 + u64::try_from(index).expect("a handful of options") * 10)
}

fn option_key(name: &str) -> Option<&'static str> {
    if name.ends_with("VarioSpeedPlus") || name.ends_with("VarioSpeed") {
        return Some("VarioSpeedPlus");
    }
    if name.ends_with("SilenceOnDemand") || name.ends_with("Silence") {
        return Some("SilenceOnDemand");
    }
    if name.ends_with("HygienePlus") || name.ends_with("Hygiene") {
        return Some("HygienePlus");
    }
    OPTIONS
        .iter()
        .find(|(key, _)| name.ends_with(key))
        .map(|(key, _)| *key)
}

/// Options this machine actually has, with the iid Home will keep.
pub fn option_switches(
    features: &BTreeMap<String, Feature>,
) -> Vec<(&'static str, u64, &'static str)> {
    OPTIONS
        .iter()
        .filter_map(|(key, label)| {
            let iid = option_iid(key)?;
            feature_uid(features, key).map(|_| (*key, iid, *label))
        })
        .collect()
}

impl State for Machine {
    fn apply(&mut self, name: &str, value: &str) {
        apply_value(self, name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::home_connect::protocol::{
        absorb, format_remaining, label_of, Command, Talk,
    };
    use serde_json::json;

    #[test]
    fn readings_become_the_fields_home_shows() {
        let mut machine = Machine::default();
        apply_value(&mut machine, "BSH.Common.Setting.PowerState", "On");
        apply_value(&mut machine, "BSH.Common.Status.DoorState", "Open");
        apply_value(&mut machine, "BSH.Common.Status.DoorState", "Locked");
        apply_value(&mut machine, "BSH.Common.Event.ProgramFinished", "Present");
        apply_value(&mut machine, "BSH.Common.Event.AquaStopOccured", "Off");
        apply_value(
            &mut machine,
            "Dishcare.Dishwasher.Event.SaltNearlyEmpty",
            "true",
        );
        apply_value(&mut machine, "Dishcare.Dishwasher.Option.ExtraDry", "On");
        apply_value(
            &mut machine,
            "BSH.Common.Option.RemainingProgramTime",
            "5400",
        );
        apply_value(&mut machine, "BSH.Common.Root.ActiveProgram", "Eco50");
        assert!(machine.power);
        assert!(!machine.door_open);
        assert!(machine.finished);
        assert!(!machine.leak);
        assert!(machine.salt_low == Some(true));
        assert_eq!(machine.options.get("ExtraDry"), Some(&true));
        assert_eq!(machine.remaining_secs, 5400);
        assert_eq!(machine.program, "Eco50");
        assert_eq!(format_remaining(5400), "1 h 30 min");
    }

    #[test]
    fn the_handshake_replies_then_sends_a_queued_command() {
        let mut features = BTreeMap::new();
        let mut values = BTreeMap::new();
        values.insert("0".into(), "Off".into());
        values.insert("1".into(), "On".into());
        features.insert(
            "512".into(),
            Feature {
                name: "BSH.Common.Setting.PowerState".into(),
                values,
            },
        );
        let mut talk = Talk::new();
        let mut machine = Machine::default();
        let hello = talk.intake(
            &features,
            &mut machine,
            r#"{"sID":7,"msgID":3,"resource":"/ei/initialValues","version":2,"action":"POST","data":[{"edMsgID":40}]}"#,
        );
        assert!(hello[0].starts_with(r#"{"sID":7,"#));
        assert!(hello[0].contains(r#""deviceType":"Application""#));
        assert!(hello[1].contains(r#""resource":"/ci/services""#));
        assert!(hello[1].contains(r#""msgID":40"#));
        assert!(talk
            .submit(
                &features,
                Command::Option {
                    key: "PowerState".into(),
                    on: true
                }
            )
            .is_none());
        let replies = talk.intake(
            &features,
            &mut machine,
            r#"{"resource":"/ci/services","action":"RESPONSE","data":[{"service":"ci","version":2},{"service":"ei","version":2},{"service":"ro","version":1}]}"#,
        );
        assert!(replies.iter().any(|line| line.contains(r#""uid":512"#)));
        talk.intake(
            &features,
            &mut machine,
            r#"{"resource":"/ro/values","action":"NOTIFY","data":[{"uid":512,"value":1}]}"#,
        );
        assert!(machine.power);
        let program = talk.submit(&features, Command::Program(30)).unwrap();
        assert!(program.contains(r#""program":30"#));
        assert!(program.find(r#""program""#).unwrap() < program.find(r#""options""#).unwrap());
        // A machine that does not list the abort command is sent nothing.
        assert!(talk.submit(&features, Command::Abort).is_none());
        features.insert(
            "600".into(),
            Feature {
                name: "BSH.Common.Command.AbortProgram".into(),
                values: BTreeMap::new(),
            },
        );
        let abort = talk.submit(&features, Command::Abort).unwrap();
        assert!(abort.contains(r#""resource":"/ro/values""#));
        assert!(abort.contains(r#""uid":600,"value":true"#));
    }

    #[test]
    fn a_programme_given_by_its_uid_is_named() {
        let feature = |name: &str| Feature {
            name: name.into(),
            values: BTreeMap::new(),
        };
        let features = BTreeMap::from([
            ("256".to_owned(), feature("BSH.Common.Root.SelectedProgram")),
            (
                "8196".to_owned(),
                feature("Dishcare.Dishwasher.Program.Eco50"),
            ),
        ]);
        let mut machine = Machine::default();
        absorb(
            &features,
            &mut machine,
            &json!([{ "uid": 256, "value": 8196 }]),
        );
        assert_eq!(label_of(&machine.program), "Eco 50");
        assert!(!machine.running());
        machine.operation = "BSH.Common.EnumType.OperationState.Run".into();
        assert!(machine.running());
    }
}
