//! One-time MyDyson login, used only to learn the fan's local MQTT password.
//!
//! The account password is sent to Dyson over HTTPS and is not written to disk.
//! What gets saved is the decrypted per-device password the fan already trusts.

use super::crypto::decrypt_local_credentials;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;

const USER_AGENT: &str = "android client";

fn api_host(country: &str) -> &'static str {
    if country.eq_ignore_ascii_case("CN") {
        "https://appapi.cp.dyson.cn"
    } else {
        "https://appapi.cp.dyson.com"
    }
}

fn culture(country: &str) -> &'static str {
    match country.to_ascii_uppercase().as_str() {
        "GB" => "en-GB",
        "DE" => "de-DE",
        "FR" => "fr-FR",
        "CN" => "zh-CN",
        _ => "en-US",
    }
}

#[derive(Debug, Clone)]
pub struct CloudDevice {
    pub name: String,
    pub serial: String,
    pub model: String,
    pub product_type: String,
    pub mqtt_password: String,
}

pub async fn login(
    email: &str,
    password: &str,
    country: &str,
    otp: &str,
) -> Result<Vec<CloudDevice>> {
    let country = country.trim().to_ascii_uppercase();
    if country.len() != 2 {
        return Err(anyhow!("country must be a two-letter code, like GB"));
    }
    let host = api_host(&country);
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let provision = format!("{host}/v1/provisioningservice/application/Android/version");
    client
        .get(&provision)
        .send()
        .await
        .context("could not reach the Dyson account service")?
        .error_for_status()
        .context("Dyson rejected the provisioning call")?;

    let status: Value = client
        .post(format!("{host}/v3/userregistration/email/userstatus"))
        .query(&[("country", country.as_str())])
        .json(&serde_json::json!({ "email": email }))
        .send()
        .await
        .context("could not check the MyDyson account")?
        .error_for_status()
        .context("MyDyson rejected that email")?
        .json()
        .await?;
    let account_status = status
        .get("accountStatus")
        .and_then(Value::as_str)
        .unwrap_or("");
    if account_status != "ACTIVE" {
        return Err(anyhow!("MyDyson account is {account_status}, not ACTIVE"));
    }

    let challenge: Value = client
        .post(format!("{host}/v3/userregistration/email/auth"))
        .query(&[
            ("country", country.as_str()),
            ("culture", culture(&country)),
        ])
        .json(&serde_json::json!({ "email": email }))
        .send()
        .await
        .context("could not request the email code")?
        .error_for_status()
        .context("Dyson would not send a login code. Wait a few minutes and try again")?
        .json()
        .await?;
    let challenge_id = challenge
        .get("challengeId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Dyson did not return a login challenge"))?;

    let verify = client
        .post(format!("{host}/v3/userregistration/email/verify"))
        .query(&[
            ("country", country.as_str()),
            ("culture", culture(&country)),
        ])
        .json(&serde_json::json!({
            "email": email,
            "password": password,
            "challengeId": challenge_id,
            "otpCode": otp.trim(),
        }))
        .send()
        .await
        .context("could not check the email code")?;
    if verify.status() == reqwest::StatusCode::UNAUTHORIZED
        || verify.status() == reqwest::StatusCode::BAD_REQUEST
    {
        return Err(anyhow!(
            "the email code or MyDyson password was not accepted"
        ));
    }
    let login: Value = verify
        .error_for_status()
        .context("login failed")?
        .json()
        .await?;
    let token = login
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("login succeeded but returned no token"))?;

    let devices: Value = client
        .get(format!("{host}/v3/manifest"))
        .bearer_auth(token)
        .send()
        .await
        .context("could not list devices on the account")?
        .error_for_status()?
        .json()
        .await?;
    let devices = devices
        .as_array()
        .ok_or_else(|| anyhow!("device list was not a list"))?;

    let mut found = Vec::new();
    for device in devices {
        let Some(mqtt) = device
            .pointer("/connectedConfiguration/mqtt")
            .filter(|value| !value.is_null())
        else {
            continue;
        };
        let Some(encrypted) = mqtt.get("localBrokerCredentials").and_then(Value::as_str) else {
            continue;
        };
        let Some(product_type) = mqtt.get("mqttRootTopicLevel").and_then(Value::as_str) else {
            continue;
        };
        let serial = device
            .get("serialNumber")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if serial.is_empty() {
            continue;
        }
        let mqtt_password = decrypt_local_credentials(encrypted)
            .with_context(|| format!("could not decrypt credentials for {serial}"))?;
        let name = device
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or("Purifier")
            .to_owned();
        let model = device
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
            .unwrap_or("HP09")
            .to_owned();
        found.push(CloudDevice {
            name,
            serial,
            model,
            product_type: product_type.to_owned(),
            mqtt_password,
        });
    }
    if found.is_empty() {
        return Err(anyhow!(
            "the account has no Wi-Fi purifier with local credentials"
        ));
    }
    Ok(found)
}
