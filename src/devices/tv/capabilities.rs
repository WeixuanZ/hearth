//! The Frame as capabilities, for the menu bar and the CLI. HomeKit shows the
//! same state through `accessory`, and both turn into the same commands.

use super::{Command, Tv};
use crate::capabilities::{Capability, Item, Set, Snapshot};
use crate::connectors::samsung::protocol::Input;
use anyhow::{anyhow, Result};

/// The remote pad: an id for the UI and CLI, a label, and the Samsung key.
const REMOTE: [(&str, &str, &str); 13] = [
    ("up", "Up", "KEY_UP"),
    ("down", "Down", "KEY_DOWN"),
    ("left", "Left", "KEY_LEFT"),
    ("right", "Right", "KEY_RIGHT"),
    ("select", "Select", "KEY_ENTER"),
    ("back", "Back", "KEY_RETURN"),
    ("home", "Home", "KEY_HOME"),
    ("play", "Play/Pause", "KEY_PLAY"),
    ("info", "Info", "KEY_INFO"),
    ("menu", "Settings", "KEY_MENU"),
    ("volume_up", "Volume up", "KEY_VOLUP"),
    ("volume_down", "Volume down", "KEY_VOLDOWN"),
    ("mute", "Mute", "KEY_MUTE"),
];

impl Tv {
    pub(super) fn snapshot(&self) -> Snapshot {
        let headline = if !self.seen {
            "Looking"
        } else if !self.reachable {
            "Unreachable"
        } else if !self.on {
            "Standby"
        } else if self.art == Some(true) {
            "Art"
        } else {
            Input::from_id(self.input).map_or("On", Input::label)
        };
        let showing = self.on && self.art != Some(true);
        let input = Input::from_id(self.input).map(|input| input.source().to_lowercase());
        Snapshot {
            id: format!("frame:{}", self.device.host),
            kind: "frame",
            category: "tv",
            name: self.device.name.clone(),
            online: self.reachable,
            headline: headline.into(),
            detail: self.device.model.clone(),
            capabilities: vec![
                Capability::power(self.on),
                Capability::switch("art", "Art mode", self.on && self.art == Some(true)),
                Capability::choice(
                    "input",
                    "Input",
                    Input::ALL
                        .iter()
                        .map(|input| Item::new(input.source().to_lowercase(), input.label()))
                        .collect(),
                    input,
                )
                .active(showing),
                Capability::level(
                    "brightness",
                    "Art brightness",
                    (0.0, 100.0, 10.0),
                    "%",
                    self.brightness.map(|level| level as f32),
                )
                .active(self.on && self.art == Some(true)),
                Capability::keys(
                    "remote",
                    "Remote",
                    REMOTE
                        .iter()
                        .map(|(id, name, _)| Item::new(*id, *name))
                        .collect(),
                )
                .active(self.on),
            ],
        }
    }
}

/// What a request asks of the TV, in the commands HomeKit sends too.
pub fn command(set: &Set) -> Result<Command> {
    Ok(match set.capability.as_str() {
        "power" => Command::Active(set.bool()?),
        "art" => Command::Art(set.bool()?),
        "input" => {
            let name = set.option()?;
            let input = Input::from_source(&name.to_uppercase())
                .ok_or_else(|| anyhow!("input takes tv or hdmi1 to hdmi4, not {name}"))?;
            Command::Input(input as u32)
        }
        "brightness" => Command::Brightness(set.number()?.round() as i32),
        "remote" => {
            let key = set.option()?;
            let (_, _, samsung) = REMOTE
                .iter()
                .find(|(id, _, _)| *id == key)
                .ok_or_else(|| anyhow!("remote has no key {key}"))?;
            Command::Remote(samsung)
        }
        _ => return Err(set.unknown()),
    })
}
