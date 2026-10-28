//! One-time MyDyson login, used only to learn the fan's local MQTT password.
//!
//! The account password is sent to Dyson over HTTPS and is not written to disk.
//! What gets saved is the decrypted per-device password the fan already trusts.

use super::crypto::decrypt_local_credentials;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;

/// The HTTP client the MyDyson Android app uses. Since October 2026, Dyson's
/// Cloudflare answers the login with 429 for the old "android client".
const USER_AGENT: &str = "okhttp/4.12.0";

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

/// Turns Dyson's 429 into advice. It can mean too many attempts, or that
/// Cloudflare blocked the request outright, so waiting may not help.
fn throttled(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(response);
    }
    let wait = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| format!(" Dyson asks to wait {seconds} seconds."))
        .unwrap_or_default();
    Err(anyhow!(
        "Dyson refused the request with 429 Too Many Requests.{wait} \
         If waiting does not help, Dyson may be blocking this client"
    ))
}

#[derive(Debug, Clone)]
pub struct CloudDevice {
    pub name: String,
    pub serial: String,
    pub model: String,
    pub product_type: String,
    pub mqtt_password: String,
}

/// A login started with [`request_code`]. Dyson has emailed the code.
pub struct Challenge {
    client: reqwest::Client,
    host: &'static str,
    country: String,
    email: String,
    challenge_id: String,
}

/// Checks the account and asks Dyson to email a login code.
pub async fn request_code(email: &str, country: &str) -> Result<Challenge> {
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
        .context("could not reach the Dyson account service")
        .and_then(throttled)?
        .error_for_status()
        .context("Dyson rejected the provisioning call")?;

    let status: Value = client
        .post(format!("{host}/v3/userregistration/email/userstatus"))
        .query(&[("country", country.as_str())])
        .json(&serde_json::json!({ "email": email }))
        .send()
        .await
        .context("could not check the MyDyson account")
        .and_then(throttled)?
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
        .context("could not request the email code")
        .and_then(throttled)?
        .error_for_status()
        .context("Dyson would not send a login code. Wait a few minutes and try again")?
        .json()
        .await?;
    let challenge_id = challenge
        .get("challengeId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Dyson did not return a login challenge"))?
        .to_owned();

    Ok(Challenge {
        client,
        host,
        country,
        email: email.to_owned(),
        challenge_id,
    })
}

/// Finishes the login with the emailed code and lists the account's fans.
pub async fn verify(challenge: Challenge, password: &str, otp: &str) -> Result<Vec<CloudDevice>> {
    let Challenge {
        client,
        host,
        country,
        email,
        challenge_id,
    } = challenge;

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
        .context("could not check the email code")
        .and_then(throttled)?;
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
        .context("could not list devices on the account")
        .and_then(throttled)?
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
