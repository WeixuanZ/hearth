mod bridge;
mod cluster;
mod config;
mod dyson;
mod frame;
mod hapset;
mod status;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{self, BufRead, Write};

#[derive(Parser)]
#[command(
    name = "hearth",
    about = "Put devices that are not HomeKit into Apple Home"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Advertise every configured device, and keep doing so.
    /// Other hearth processes on this network share the work.
    Run,
    /// The devices the menu can add.
    Products,
    /// Stop showing a device in Home. Other machines drop it too.
    Remove {
        /// The device's name.
        name: String,
    },
    /// Read each device once. Does not start HomeKit.
    Status,
    /// Print the pairing code Apple Home will ask for.
    Pin,
    /// Rename a device, as Home will show it after the bridge restarts.
    Rename {
        /// The device's current name.
        from: String,
        /// The new name.
        to: String,
    },
    /// Forget Home's pairing, so the bridge can be added again. Use this when
    /// the bridge was removed from Home but does not show up to add.
    Unpair,
    /// A Dyson purifier.
    Dyson {
        #[command(subcommand)]
        command: DysonCommand,
    },
    /// A Samsung The Frame television.
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
        /// Two-letter country of the MyDyson account. Defaults to GB.
        #[arg(long, default_value = "GB")]
        country: String,
        /// Serial, if the account has more than one fan.
        #[arg(long)]
        serial: Option<String>,
    },
    /// Ask Dyson to email a login code. The menu uses this, then `submit`.
    Request {
        #[arg(long)]
        email: String,
        #[arg(long, default_value = "GB")]
        country: String,
    },
    /// Finish a login started with `request`. Reads the password and the code.
    Submit {
        #[arg(long)]
        serial: Option<String>,
    },
    /// Send one command, as Home would, like `power on` or `speed 4`.
    /// Stop the bridge first.
    Set { what: String, value: String },
}

#[derive(Subcommand)]
enum FrameCommand {
    /// Press Allow on the TV. Saves the token it hands back.
    Pair {
        /// The TV's address. Found on the network if you leave this out.
        #[arg(long)]
        host: Option<String>,
        /// Name in Apple Home. Defaults to The Frame.
        #[arg(long, default_value = "The Frame")]
        name: String,
    },
    /// Press remote keys in order, like KEY_POWER or KEY_HDMI1.
    Key {
        #[arg(required = true)]
        keys: Vec<String>,
        /// Hold each key down for this many seconds, as a long press.
        #[arg(long)]
        hold: Option<f32>,
    },
    /// Turn the TV on or off, as Home does.
    Power {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Turn art mode on or off, as Home does.
    Art {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Switch to HDMI 1–4, or `tv` for the TV's own source, as Home does.
    Input {
        #[arg(value_parser = ["tv", "1", "2", "3", "4"])]
        source: String,
    },
    /// Print the TV's power, art mode and input.
    Info,
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "hearth=info");
    }
    env_logger::init();

    match Cli::parse().command {
        Command::Run => cluster::run().await?,
        Command::Products => {
            println!(
                "{}",
                serde_json::json!([
                    {"id": "dyson-hp09", "kind": "dyson", "name": "Dyson Purifier Hot+Cool Formaldehyde"},
                    {"id": "frame-2024", "kind": "frame", "name": "Samsung The Frame"}
                ])
            );
        }
        Command::Remove { name } => {
            let mut config = config::load()?;
            config::remove_device(&mut config, &name)?;
            config::save(&config)?;
            println!("Removed {name}. The bridge picks that up within a few seconds.");
        }
        Command::Status => {
            let config = config::load()?;
            for device in &config.devices {
                if let Err(error) = status(device).await {
                    println!("{}: {error:#}", device.name());
                }
            }
        }
        Command::Pin => {
            let config = config::load()?;
            println!("{}", config.homekit_pin);
        }
        Command::Rename { from, to } => {
            let mut config = config::load()?;
            config::rename(&mut config, &from, &to)?;
            config::save(&config)?;
            println!(
                "Renamed {from} to {}. Restart the bridge to show it in Home.",
                to.trim()
            );
        }
        Command::Unpair => {
            let removed = config::clear_pairings()?;
            // A new identity too. Home may still hold the old one, if it was
            // removed while the bridge was off, and would refuse it as
            // already in a home.
            let mut config = config::load()?;
            config.bridge_id = format!("hearth-{:016x}", rand::random::<u64>());
            config.updated = config::now_secs();
            config::save(&config)?;
            println!("Forgot {removed} pairing(s), and took a new HomeKit identity.");
            println!("Restart the bridge, then in Home: Add Accessory → More options… → Hearth.");
            println!("If Home still shows an old Hearth, remove it there.");
            println!("Code: {}", config.homekit_pin);
        }
        Command::Dyson { command } => match command {
            DysonCommand::Login {
                email,
                country,
                serial,
            } => dyson::login(email, country, serial).await?,
            DysonCommand::Request { email, country } => dyson::request(email, country).await?,
            DysonCommand::Submit { serial } => {
                let password = secret("MyDyson password: ")?;
                let code = secret("Email code: ")?;
                dyson::submit(password, code, serial).await?
            }
            DysonCommand::Set { what, value } => dyson::set(&what, &value).await?,
        },
        Command::Frame { command } => match command {
            FrameCommand::Pair { host, name } => frame::pair(host, name).await?,
            FrameCommand::Key { keys, hold } => frame::keys(keys, hold).await?,
            FrameCommand::Power { state } => {
                frame::control(Some(frame::Command::Active(state == "on"))).await?
            }
            FrameCommand::Art { state } => {
                frame::control(Some(frame::Command::Art(state == "on"))).await?
            }
            // HDMI 1 is input id 2. See frame::protocol::Input.
            FrameCommand::Input { source } => {
                frame::control(Some(frame::Command::Input(frame::input_id(&source)))).await?
            }
            FrameCommand::Info => frame::control(None).await?,
        },
    }
    Ok(())
}

async fn status(device: &config::Device) -> Result<()> {
    match device {
        config::Device::Dyson(device) => dyson::status_timeout(device).await,
        config::Device::Frame(device) => frame::status(device).await,
    }
}

pub fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    read_required()
}

/// A prompt on a terminal, or one line from the menu.
fn secret(label: &str) -> Result<String> {
    if io::IsTerminal::is_terminal(&io::stdin()) {
        if label.contains("password") {
            return Ok(rpassword::prompt_password(label)?);
        }
        return prompt(label);
    }
    read_required()
}

fn read_required() -> Result<String> {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let line = line.trim().to_owned();
    if line.is_empty() {
        anyhow::bail!("that was empty");
    }
    Ok(line)
}
