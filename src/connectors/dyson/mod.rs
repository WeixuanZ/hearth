//! Dyson, over the fan's own MQTT on the LAN.
//!
//! The account is used once, by `hearth dyson login`, to learn the fan's local
//! password. After that nothing here talks to Dyson.

mod cloud;
mod crypto;
pub mod discover;
pub mod mqtt;
pub mod protocol;

use crate::config::{self, DysonDevice};
use anyhow::{anyhow, Result};
use cloud::CloudDevice;

/// The product's own name, the default in Home. A MyDyson name is often a
/// room, which Home drops from the start of an accessory's name.
pub fn product_name(model: &str) -> String {
    let product = match model {
        "HP09" => "Purifier Hot+Cool Formaldehyde",
        hot if hot.starts_with("HP") => "Purifier Hot+Cool",
        humid if humid.starts_with("PH") => "Purifier Humidify+Cool",
        _ => "Purifier Cool",
    };
    format!("Dyson {product}")
}

pub async fn login(email: String, country: String, serial: Option<String>) -> Result<()> {
    let challenge = cloud::request_code(&email, &country).await?;
    println!("Dyson has emailed a code to {email}.");
    let password = rpassword::prompt_password("MyDyson password: ")?;
    let otp = crate::prompt("Email code: ")?;
    let devices = cloud::verify(challenge, &password, &otp).await?;
    let device = choose(devices, serial)?;

    let mut saved = config::load_or_new()?;
    if saved.bridge_id.is_empty() {
        saved.bridge_id = device.serial.clone();
    }
    config::upsert_dyson(
        &mut saved,
        DysonDevice {
            aid: 0,
            name: product_name(&device.model),
            serial: device.serial,
            model: device.model,
            product_type: device.product_type,
            mqtt_password: device.mqtt_password,
            host: String::new(),
            port: 1883,
            heating: true,
        },
    );
    let path = config::save(&saved)?;
    println!();
    println!("Saved {}.", path.display());
    println!("Apple Home pairing code: {}", saved.homekit_pin);
    println!("Next: hearth run");
    Ok(())
}

fn choose(devices: Vec<CloudDevice>, serial: Option<String>) -> Result<CloudDevice> {
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
    let serial = crate::prompt("Serial to put in Apple Home: ")?;
    devices
        .into_iter()
        .find(|device| device.serial.eq_ignore_ascii_case(serial.trim()))
        .ok_or_else(|| anyhow!("that serial is not on the account"))
}
