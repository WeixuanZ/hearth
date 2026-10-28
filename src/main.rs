mod bridge;
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
    Run,
    /// Read each device once. Does not start HomeKit.
    Status,
    /// Print the pairing code Apple Home will ask for.
    Pin,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "hearth=info");
    }
    env_logger::init();

    match Cli::parse().command {
        Command::Run => {
            let config = config::load()?;
            if config.devices.is_empty() {
                anyhow::bail!("no devices yet. Run `hearth dyson login` or `hearth frame pair`");
            }
            bridge::serve(config).await?;
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
        Command::Dyson { command } => match command {
            DysonCommand::Login {
                email,
                country,
                serial,
            } => dyson::login(email, country, serial).await?,
        },
        Command::Frame { command } => match command {
            FrameCommand::Pair { host, name } => frame::pair(host, name).await?,
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
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let line = line.trim().to_owned();
    if line.is_empty() {
        anyhow::bail!("that was empty");
    }
    Ok(line)
}
