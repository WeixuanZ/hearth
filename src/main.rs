mod bus;
mod capabilities;
mod cluster;
mod config;
mod connectors;
mod devices;
mod driver;
mod run;
mod series;
mod setup;
mod surfaces;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use std::io::{self, BufRead, Write};
use surfaces::cli;

/// Shown under `hearth --help`, and each parsed by a test, so none goes stale.
const EXAMPLES: &[(&str, &str)] = &[
    ("hearth devices", "List devices and what each accepts"),
    ("hearth get tv", "Show the TV"),
    ("hearth set tv power on", ""),
    ("hearth set tv input hdmi2", ""),
    ("hearth set tv remote select", "Press a key"),
    ("hearth set purifier speed 4", ""),
    (
        "hearth set dishwasher programme \"Eco 50\"",
        "Choose, sends nothing",
    ),
    ("hearth set dishwasher run start", "Start it, or stop"),
    ("hearth get purifier temperature --json", ""),
    ("hearth history purifier temperature", ""),
    ("hearth history purifier --hours 168 --json", ""),
    (
        "hearth history purifier temperature --usual",
        "Its usual day",
    ),
    (
        "hearth rename tv \"Living room TV\"",
        "Renames it in Home too",
    ),
    ("hearth doctor", "What stands between Home and the devices"),
    ("hearth events", "Follow changes as they happen"),
];

const NOTES: &str = "\
A device is its id, name, connector (dyson, frame, bosch), kind (purifier, tv,
dishwasher), or a part of its name. A value is on or off, start or stop, a
number, or an option id listed by `hearth devices`. Most commands talk to the
running bridge; history also reads the saved files when it is stopped.";

/// The CLI as clap sees it, with the examples. `hearth mcp` builds its tools
/// from this too.
fn command() -> clap::Command {
    let width = EXAMPLES
        .iter()
        .map(|(line, _)| line.len())
        .max()
        .unwrap_or(0);
    let examples: String = EXAMPLES
        .iter()
        .map(|(line, note)| format!("  {line:width$}  {note}").trim_end().to_owned() + "\n")
        .collect();
    Cli::command().after_help(format!("Examples:\n{examples}\n{NOTES}"))
}

#[derive(Parser)]
#[command(
    name = "hearth",
    version,
    about = "Put devices that are not HomeKit into Apple Home"
)]
struct Cli {
    /// Print JSON, for scripts and agents.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List every device, each capability's value, and what it accepts.
    Devices,
    /// Show a device, or one of its capabilities.
    Get {
        /// Id, name, connector, kind, or part of a name.
        device: String,
        /// Like power, input or temperature. All of them if left out.
        capability: Option<String>,
    },
    /// Show readings over time: temperature, humidity, air quality.
    History {
        /// Id, name, connector, kind, or part of a name.
        device: String,
        /// Like temperature or pm25. All readings if left out.
        capability: Option<String>,
        /// How far back to look, up to 720 (30 days).
        #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u64).range(1..=720))]
        hours: u64,
        /// The usual day instead, from the last 30: each hour's range and mean, with today.
        #[arg(long, requires = "capability", conflicts_with = "hours")]
        usual: bool,
    },
    /// Follow what happens as it happens: each change, and alerts raised and cleared.
    Events {
        /// Only topics starting with this, like hearth/bosch: for one device.
        #[arg(long, default_value = "")]
        topic: String,
    },
    /// Change a capability, like power, an input or a speed.
    Set {
        /// Id, name, connector, kind, or part of a name.
        device: String,
        /// Like power, input or speed. See `hearth devices`.
        capability: String,
        /// on, off, a number, or an option id.
        value: String,
    },
    /// Run the bridge: serve Apple Home, the menu bar and this CLI, with the
    /// other computers in the group.
    Run,
    /// Check what Home and the devices need: the bridge, the network, the firewall, each device.
    Doctor,
    /// Serve the devices to AI agents over MCP, on stdin and stdout.
    Mcp,
    /// Print the Apple Home pairing code.
    Pin,
    /// Rename a device, in the menu bar, the CLI and Home.
    Rename {
        /// The device: as for `get`, or its current name if the bridge is off.
        device: String,
        /// The new name.
        name: String,
    },
    /// Remove a device, from Home and from every computer in the group.
    Remove {
        /// Its id or name.
        device: String,
    },
    /// Show the computers sharing this bridge, or act on a request to join.
    Group {
        #[command(subcommand)]
        command: Option<GroupCommand>,
    },
    /// Forget Apple Home, and take a new HomeKit identity. For a bridge
    /// Home will not add again.
    Unpair,
    /// Add a device: dyson, frame or bosch. Asks what it needs.
    Add {
        /// dyson, frame or bosch.
        product: String,
    },
    /// Add a Dyson purifier.
    Dyson {
        #[command(subcommand)]
        command: DysonCommand,
    },
    /// Add a Samsung The Frame television.
    Frame {
        #[command(subcommand)]
        command: FrameCommand,
    },
    /// Add a Bosch dishwasher.
    Bosch {
        #[command(subcommand)]
        command: BoschCommand,
    },
}

#[derive(Subcommand)]
enum DysonCommand {
    /// Learn the fan's local password from MyDyson. This talks to Dyson once.
    Login {
        #[arg(long)]
        email: String,
        /// Two-letter country of the MyDyson account.
        #[arg(long, default_value = "GB")]
        country: String,
        /// Serial, if the account has more than one fan.
        #[arg(long)]
        serial: Option<String>,
    },
}

#[derive(Subcommand)]
enum GroupCommand {
    /// Let in the computer asking to join: type the code it shows.
    Approve { code: String },
    /// Turn away the computer asking to join.
    Decline,
    /// Say you typed this computer's code on the other one, to finish joining.
    Confirm,
    /// Stop asking to join, and keep a group of this computer's own.
    Separate,
    /// Leave this group for the other one on the network.
    Join,
}

#[derive(Subcommand)]
enum FrameCommand {
    /// Pair the TV: press Allow on it, twice.
    Pair {
        /// The TV's address. Found on the network if left out.
        #[arg(long)]
        host: Option<String>,
        /// Name in Apple Home.
        #[arg(long, default_value = "The Frame")]
        name: String,
    },
}

#[derive(Subcommand)]
enum BoschCommand {
    /// Sign in to Home Connect once and save the dishwasher's local key.
    Login {
        /// Address, if the name Home Connect gives does not resolve.
        #[arg(long)]
        host: Option<String>,
        /// Appliance id, if the account has more than one.
        #[arg(long)]
        id: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "hearth=info");
    }
    env_logger::init();

    let cli = Cli::from_arg_matches(&command().get_matches()).unwrap_or_else(|error| error.exit());
    match cli.command {
        Command::Devices => cli::devices(cli.json).await?,
        Command::Get { device, capability } => {
            cli::get(&device, capability.as_deref(), cli.json).await?
        }
        Command::History {
            device,
            capability: Some(capability),
            usual: true,
            ..
        } => cli::usual(&device, &capability, cli.json).await?,
        Command::History {
            device,
            capability,
            hours,
            ..
        } => cli::history(&device, capability.as_deref(), hours, cli.json).await?,
        Command::Set {
            device,
            capability,
            value,
        } => cli::set(&device, &capability, &value, cli.json).await?,
        Command::Events { topic } => cli::events(&topic, cli.json).await?,
        Command::Run => run::run().await?,
        Command::Doctor => surfaces::doctor::run(cli.json).await?,
        Command::Mcp => surfaces::mcp::serve(command()).await?,
        Command::Pin => println!("{}", config::load()?.homekit_pin),
        Command::Rename { device, name } => cli::rename(&device, &name, cli.json).await?,
        Command::Remove { device } => {
            let mut config = config::load()?;
            let name = config::remove(&mut config, &device)?;
            config::save(&config)?;
            println!("Removed {name}.");
        }
        Command::Group { command } => {
            let op = command.map(|command| match command {
                GroupCommand::Approve { code } => {
                    serde_json::json!({ "op": "approve", "code": code })
                }
                GroupCommand::Decline => serde_json::json!({ "op": "decline" }),
                GroupCommand::Confirm => serde_json::json!({ "op": "confirm" }),
                GroupCommand::Separate => serde_json::json!({ "op": "separate" }),
                GroupCommand::Join => serde_json::json!({ "op": "join" }),
            });
            cli::group(op, cli.json).await?
        }
        Command::Unpair => {
            let removed = config::clear_pairings()?;
            // A new identity too. Home may still hold the old one, if it was
            // removed while the bridge was off, and would refuse it as
            // already in a home.
            let mut config = config::load()?;
            config.bridge_id = config::new_bridge_id();
            config::save(&config)?;
            println!("Forgot {removed} pairing(s), and took a new HomeKit identity.");
            println!("Restart the bridge, then in Home: Add Accessory → More options… → Hearth.");
            println!("If Home still shows an old Hearth, remove it there.");
            println!("Code: {}", config.homekit_pin);
        }
        Command::Add { product } => setup::in_terminal(&product, setup::Answers::new()).await?,
        // The same steps as `add`, with what the flags already answer.
        Command::Dyson {
            command:
                DysonCommand::Login {
                    email,
                    country,
                    serial,
                },
        } => {
            let mut given = answers([("email", Some(email)), ("country", Some(country))]);
            given.extend(answers([("serial", serial)]));
            setup::in_terminal("dyson", given).await?
        }
        Command::Frame {
            command: FrameCommand::Pair { host, name },
        } => {
            let given = answers([
                ("name", Some(name)),
                ("host", Some(host.unwrap_or_default())),
            ]);
            setup::in_terminal("frame", given).await?
        }
        Command::Bosch {
            command: BoschCommand::Login { host, id },
        } => {
            let mut given = answers([("host", Some(host.unwrap_or_default()))]);
            given.extend(answers([("appliance", id)]));
            setup::in_terminal("bosch", given).await?
        }
    }
    Ok(())
}

/// Answers for a setup, from the flags that were given.
fn answers<const N: usize>(pairs: [(&str, Option<String>); N]) -> setup::Answers {
    pairs
        .into_iter()
        .filter_map(|(id, value)| Some((id.to_owned(), value?)))
        .collect()
}

pub fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let line = line.trim().to_owned();
    if line.is_empty() {
        anyhow::bail!("that was empty");
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words as a shell splits them, double quotes holding spaces.
    fn words(line: &str) -> Vec<String> {
        let mut words = vec![String::new()];
        let mut quoted = false;
        for character in line.chars() {
            match character {
                '"' => quoted = !quoted,
                ' ' if !quoted => words.push(String::new()),
                other => words.last_mut().unwrap().push(other),
            }
        }
        words.retain(|word| !word.is_empty());
        words
    }

    fn parses(line: &str) {
        if let Err(error) = command().try_get_matches_from(words(line)) {
            panic!("`{line}` does not parse: {error}");
        }
    }

    #[test]
    fn every_example_is_a_command_that_parses() {
        command().debug_assert();
        EXAMPLES.iter().for_each(|(line, _)| parses(line));
    }

    #[test]
    fn every_command_in_the_readme_parses() {
        let readme = include_str!("../README.md");
        let mut in_code = false;
        let lines: Vec<&str> = readme
            .lines()
            .map(str::trim)
            .filter(|line| {
                if line.starts_with("```") {
                    in_code = !in_code;
                }
                in_code && line.starts_with("hearth ")
            })
            .map(|line| line.split(" #").next().unwrap_or(line).trim())
            .collect();
        assert!(lines.len() > 10, "the README's examples were not found");
        lines.into_iter().for_each(parses);
    }
}
