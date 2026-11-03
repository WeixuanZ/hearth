//! The Frame as capabilities, for the menu bar and the CLI. HomeKit shows the
//! same state through `accessory`, and both turn into the same commands.

use super::protocol::Input;
use super::{Command, Tv};
use crate::capabilities::{About, Capability, Item, Role, Set, Snapshot};
use crate::config::Settings as _;
use anyhow::{anyhow, Result};

/// The input that is the TV's home screen: picking it presses Home.
pub(super) const HOME_SCREEN: &str = "home";
/// Its number for Home, past any real input's.
const HOME_NUMBER: u32 = 100;

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

impl<L> Tv<L> {
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
            id: self.device.id(),
            kind: <super::SamsungFrame as crate::driver::Driver>::KIND,
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
                        .map(|input| {
                            Item::new(input.source().to_lowercase(), input.label())
                                .numbered(*input as u32)
                        })
                        // Not an input: its Home key, which Home's remote
                        // and the Control Center's have no button for.
                        .chain([Item::new(HOME_SCREEN, "Home screen").numbered(HOME_NUMBER)])
                        .collect(),
                    input,
                )
                .role(Role::Input)
                .active(showing),
                Capability::level(
                    "brightness",
                    "Art brightness",
                    (0.0, 100.0, 10.0),
                    "%",
                    self.brightness.map(|level| level as f32),
                )
                .role(Role::Brightness)
                .active(self.on && self.art == Some(true)),
                Capability::keys(
                    "remote",
                    "Remote",
                    REMOTE
                        .iter()
                        .map(|(id, name, _)| Item::new(*id, *name))
                        .collect(),
                )
                .role(Role::Remote)
                .active(self.on),
                // For whatever text box the TV shows, as its own keyboard.
                Capability::text("text", "Type").active(showing),
            ],
            alerts: Vec::new(),
            about: About {
                maker: "Samsung".into(),
                model: if self.device.model.is_empty() {
                    "The Frame 2024".into()
                } else {
                    self.device.model.clone()
                },
                serial: if self.device.mac.is_empty() {
                    self.device.host.clone()
                } else {
                    self.device.mac.clone()
                },
                firmware: "2024".into(),
            },
        }
    }
}

/// What a request asks of the TV, in the commands HomeKit sends too.
pub fn command(set: &Set) -> Result<Command> {
    Ok(match set.capability.as_str() {
        "power" => Command::Active(set.bool()?),
        "art" => Command::Art(set.bool()?),
        "input" if set.option()? == HOME_SCREEN => Command::Remote("KEY_HOME"),
        "input" => {
            let name = set.option()?;
            let input = Input::from_source(&name.to_uppercase())
                .ok_or_else(|| anyhow!("input takes tv or hdmi1 to hdmi4, not {name}"))?;
            Command::Input(input as u32)
        }
        "brightness" => Command::Brightness(set.number()?.round() as i32),
        "text" => Command::Text(
            set.value
                .as_str()
                .ok_or_else(|| anyhow!("text takes words, in quotes"))?
                .to_owned(),
        ),
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
