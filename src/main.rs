mod cloud;
mod config;
mod crypto;
mod discover;
mod homekit;
mod mqtt;
mod protocol;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use protocol::{
    command_topic, intent_fields, request_environment_payload, request_state_payload, status_topic,
    Intent, MachineState, DEFAULT_PRODUCT_TYPES,
};
use std::{
    io::{self, BufRead, Write},
    time::Duration,
};
use tokio::sync::{mpsc, watch};

#[derive(Parser)]
#[command(
    name = "dyson-home",
    about = "Show a Dyson Purifier Hot+Cool Formaldehyde in Apple Home"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    /// Advertise the fan to Apple Home, and keep doing so.
    Run,
    /// Read the fan once and print it. Does not start HomeKit.
    Status,
    /// Print the pairing code Apple Home will ask for.
    Pin,
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "dyson_home=info");
    }
    env_logger::init();

    match Cli::parse().command {
        Command::Login {
            email,
            country,
            serial,
        } => login(email, country, serial).await?,
        Command::Run => run(false).await?,
        Command::Status => run(true).await?,
        Command::Pin => {
            let config = config::load()?;
            println!("{}", config.homekit_pin);
        }
    }
    Ok(())
}

async fn login(email: String, country: String, serial: Option<String>) -> Result<()> {
    let password = rpassword::prompt_password("MyDyson password: ")?;
    println!("Dyson is emailing a code to {email}.");
    let otp = prompt("Email code: ")?;
    let devices = cloud::login(&email, &password, &country, &otp).await?;
    let device = choose_device(devices, serial)?;

    let mut saved = config::load().unwrap_or_else(|_| config::Config {
        name: device.name.clone(),
        serial: device.serial.clone(),
        model: device.model.clone(),
        product_type: device.product_type.clone(),
        mqtt_password: device.mqtt_password.clone(),
        host: String::new(),
        port: 1883,
        homekit_pin: config::format_pin(&config::random_pin()),
        heating: true,
    });
    saved.name = device.name;
    saved.serial = device.serial;
    saved.model = device.model;
    saved.product_type = device.product_type;
    saved.mqtt_password = device.mqtt_password;
    let path = config::save(&saved)?;
    println!();
    println!("Saved {}.", path.display());
    println!(
        "Fan {} ({}) on MQTT topic {}.",
        saved.name, saved.serial, saved.product_type
    );
    println!("Apple Home pairing code: {}", saved.homekit_pin);
    println!("Next, on this Mac: dyson-home run");
    Ok(())
}

fn choose_device(
    devices: Vec<cloud::CloudDevice>,
    serial: Option<String>,
) -> Result<cloud::CloudDevice> {
    if let Some(serial) = serial {
        return devices
            .into_iter()
            .find(|device| device.serial.eq_ignore_ascii_case(serial.trim()))
            .ok_or_else(|| anyhow!("no fan on the account has serial {serial}"));
    }
    if devices.len() == 1 {
        return Ok(devices.into_iter().next().unwrap());
    }
    println!("This account has more than one fan:");
    for device in &devices {
        println!(
            "  {}  {}  {}  topic {}",
            device.serial, device.model, device.name, device.product_type
        );
    }
    let serial = prompt("Serial to put in Apple Home: ")?;
    devices
        .into_iter()
        .find(|device| device.serial.eq_ignore_ascii_case(serial.trim()))
        .ok_or_else(|| anyhow!("that serial is not on the account"))
}

fn prompt(label: &str) -> Result<String> {
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

fn product_types(config: &config::Config) -> Vec<String> {
    let configured = config.product_type.trim();
    if configured.is_empty() || configured.eq_ignore_ascii_case("auto") {
        DEFAULT_PRODUCT_TYPES
            .iter()
            .map(|kind| (*kind).to_owned())
            .collect()
    } else {
        vec![configured.to_owned()]
    }
}

async fn resolve_host(config: &config::Config) -> Result<String> {
    if !config.host.trim().is_empty() {
        return Ok(config.host.trim().to_owned());
    }
    log::info!("looking for {} on the network", config.serial);
    discover::find_host(&config.serial, Duration::from_secs(12)).await
}

async fn run(status_only: bool) -> Result<()> {
    let config = config::load()?;
    let host = resolve_host(&config).await?;
    log::info!("purifier is at {host}:{}", config.port);

    let (intent_tx, mut intent_rx) = mpsc::channel::<Intent>(32);
    let (state_tx, state_rx) = watch::channel(MachineState::default());
    let (command_tx, mut command_rx) = mpsc::channel::<mqtt::Publish>(32);
    let (event_tx, mut event_rx) = mpsc::channel::<mqtt::Incoming>(32);

    let serial = config.serial.clone();
    let password = config.mqtt_password.clone();
    let port = config.port;
    let topics: Vec<String> = product_types(&config)
        .iter()
        .map(|kind| status_topic(kind, &serial))
        .collect();
    let host_for_session = host.clone();
    let serial_for_session = serial.clone();
    tokio::spawn(async move {
        loop {
            if let Err(error) = mqtt::session(
                &host_for_session,
                port,
                &serial_for_session,
                &password,
                &topics,
                &mut command_rx,
                &event_tx,
            )
            .await
            {
                log::warn!("fan connection dropped: {error:#}");
            }
            if event_tx.is_closed() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });

    let types = product_types(&config);
    let fan = async move {
        let mut state = MachineState::default();
        let mut prefix = if types.len() == 1 {
            Some(types[0].clone())
        } else {
            None
        };
        let mut poll = tokio::time::interval(Duration::from_secs(30));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                incoming = event_rx.recv() => {
                    let Some(incoming) = incoming else { break };
                    if protocol::apply_message(&mut state, &incoming.topic, &incoming.payload) {
                        if state.topic_prefix.is_some() {
                            prefix = state.topic_prefix.clone();
                        }
                        state_tx.send(state.clone()).ok();
                        if status_only && state.temperature_c.is_some() {
                            print_status(&state);
                            break;
                        }
                    }
                }
                intent = intent_rx.recv() => {
                    let Some(intent) = intent else { break };
                    publish(&command_tx, &serial, prefix.as_deref(), &state, &intent).await;
                }
                _ = poll.tick() => {
                    let targets: Vec<String> = match &prefix {
                        Some(prefix) => vec![prefix.clone()],
                        None => types.clone(),
                    };
                    for kind in targets {
                        let _ = command_tx.send(mqtt::Publish {
                            topic: command_topic(&kind, &serial),
                            payload: request_state_payload(),
                        }).await;
                        let _ = command_tx.send(mqtt::Publish {
                            topic: command_topic(&kind, &serial),
                            payload: request_environment_payload(),
                        }).await;
                    }
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    if status_only {
        let fan = fan;
        tokio::time::timeout(Duration::from_secs(20), fan)
            .await
            .context("the fan did not answer within 20 seconds")??;
        return Ok(());
    }

    println!("Apple Home pairing code: {}", config.homekit_pin);
    println!(
        "In the Home app: Add Accessory, More options, then {}.",
        config.name
    );
    println!("The Mac has to stay awake and on this Wi-Fi. A sleeping Mac drops out of Home.");

    let home = homekit::serve(config, state_rx, intent_tx);
    tokio::select! {
        result = fan => result?,
        result = home => result?,
        _ = tokio::signal::ctrl_c() => log::info!("stopping"),
    }
    Ok(())
}

async fn publish(
    commands: &mpsc::Sender<mqtt::Publish>,
    serial: &str,
    prefix: Option<&str>,
    state: &MachineState,
    intent: &Intent,
) {
    let Some(prefix) = prefix else {
        log::warn!("not sending a command until the fan has identified its topic");
        return;
    };
    let fields = intent_fields(state, intent);
    let _ = commands
        .send(mqtt::Publish {
            topic: command_topic(prefix, serial),
            payload: protocol::state_set_payload(&fields),
        })
        .await;
}

fn print_status(state: &MachineState) {
    println!(
        "power {}  auto {}  speed {}  swing {}  night {}  focus {}  heat {}",
        on_off(state.power_on),
        on_off(state.auto),
        state
            .speed
            .map(|speed| speed.to_string())
            .unwrap_or_else(|| "-".into()),
        on_off(state.oscillating),
        on_off(state.night),
        on_off(state.focus),
        on_off(state.heat_on),
    );
    println!(
        "temp {}  humidity {}  PM2.5 {}  PM10 {}  formaldehyde {} mg/m³  HEPA {}%",
        state
            .temperature_c
            .map(|n| format!("{n:.1}°C"))
            .unwrap_or_else(|| "-".into()),
        state
            .humidity
            .map(|n| format!("{n:.0}%"))
            .unwrap_or_else(|| "-".into()),
        state
            .pm25
            .map(|n| format!("{n:.0}"))
            .unwrap_or_else(|| "-".into()),
        state
            .pm10
            .map(|n| format!("{n:.0}"))
            .unwrap_or_else(|| "-".into()),
        state
            .hcho_mg_m3
            .map(|n| format!("{n:.3}"))
            .unwrap_or_else(|| "-".into()),
        state
            .hepa_life
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into()),
    );
}

fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}
