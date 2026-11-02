//! What every Dyson product shares: the MyDyson account, used once when a
//! fan is added to learn its local password; finding it on the network; and
//! how its MQTT topics and messages are framed. After setup nothing here
//! talks to Dyson. Each model's own protocol lives with the model.

pub mod cloud;
pub mod crypto;
pub mod discover;
pub mod messages;

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

#[cfg(test)]
mod tests {
    #[test]
    fn a_model_has_its_product_name() {
        assert_eq!(
            super::product_name("HP09"),
            "Dyson Purifier Hot+Cool Formaldehyde"
        );
        assert_eq!(super::product_name("HP07"), "Dyson Purifier Hot+Cool");
        assert_eq!(super::product_name("XY01"), "Dyson Purifier Cool");
    }
}
