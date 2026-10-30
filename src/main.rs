mod capabilities;
mod cluster;
mod config;
mod connectors;
mod devices;
mod run;
mod surfaces;

use anyhow::Result;
use clap::{Parser, Subcommand};
use connectors::{dyson, samsung};
use std::io::{self, BufRead, Write};
use surfaces::cli;

const EXAMPLES: &str = "\
Examples:
  hearth devices                     List devices and what each accepts
  hearth get tv                      Show the TV
  hearth set tv power on
  hearth set tv input hdmi2
  hearth set tv remote select
  hearth set purifier speed 4
  hearth set purifier sweep 90
  hearth get purifier temperature --json
  hearth rename tv \"Living room TV\"  Renames it in Home too
  hearth group                       Which computer serves Home

A device is its id, name, connector (dyson, frame), kind (purifier, tv), or a
part of its name. A value is on or off, a number, or an option id listed by
`hearth devices`. devices, get and set talk to the running bridge.";

#[derive(Parser)]
#[command(
    name = "hearth",
    version,
    about = "Put devices that are not HomeKit into Apple Home",
    after_help = EXAMPLES
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

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "hearth=info");
    }
    env_logger::init();

    let cli = Cli::parse();
    match cli.command {
        Command::Devices => cli::devices(cli.json).await?,
        Command::Get { device, capability } => {
            cli::get(&device, capability.as_deref(), cli.json).await?
        }
        Command::Set {
            device,
            capability,
            value,
        } => cli::set(&device, &capability, &value, cli.json).await?,
        Command::Run => run::run().await?,
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
        Command::Dyson {
            command:
                DysonCommand::Login {
                    email,
                    country,
                    serial,
                },
        } => dyson::login(email, country, serial).await?,
        Command::Frame {
            command: FrameCommand::Pair { host, name },
        } => samsung::pair(host, name).await?,
    }
    Ok(())
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
