//! Dyson, over the fan's own MQTT on the LAN.
//!
//! The account is used once, by `setup`, to learn the fan's local password.
//! After that nothing here talks to Dyson.

mod cloud;
mod crypto;
pub mod discover;
pub mod protocol;
mod setup;

pub use setup::Setup;

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
