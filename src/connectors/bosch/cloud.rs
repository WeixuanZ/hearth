//! One Home Connect sign-in.
//!
//! The account password is typed into Home Connect's own page. The PKCE
//! verifier and the access token live in memory, only while the sign-in
//! lasts; nothing but the dishwasher's local key is written down.

use super::protocol::label_of;
use super::xml;
use crate::config::{self, BoschDevice};
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE, Engine};
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;

const CLIENT_ID: &str = "9B75AC9EC512F36C84256AC47D813E2C1DD0D6520DF774B020E1E6E2EB29B1F3";
const REDIRECT: &str = "hcauth://auth/prod";
const AUTHORIZE: &str = "https://api.home-connect.com/security/oauth/authorize";
const TOKEN: &str = "https://api.home-connect.com/security/oauth/token";
const REGIONS: &[&str] = &[
    "https://eu.services.home-connect.com",
    "https://na.services.home-connect.com",
];

/// A sign-in under way, until Home Connect sends the browser back.
pub struct SignIn {
    verifier: String,
    state: String,
    nonce: String,
}

impl SignIn {
    pub fn new() -> Self {
        Self {
            verifier: random_b64(32),
            state: random_b64(16),
            nonce: random_b64(16),
        }
    }

    /// Home Connect's own sign-in page.
    pub fn address(&self) -> String {
        self.authorize_url()
    }

    /// Trades the address the browser was sent to for the account's dishwashers.
    pub async fn finish(&self, redirect: &str) -> Result<Account> {
        let (code, state) = parse_redirect(redirect)?;
        if state.is_some_and(|state| state != self.state) {
            anyhow::bail!("that address is from another sign-in. Start again");
        }
        let client = http()?;
        let token = access_token(&client, self, &code).await?;
        let account = subject(&token)?;
        let (region, body) = appliances(&client, &token, &account).await?;
        let dishwashers = candidates(parse_appliances(&body), None)?;
        Ok(Account {
            client,
            token,
            region,
            dishwashers,
        })
    }

    fn authorize_url(&self) -> String {
        format!(
            "{AUTHORIZE}?response_type=code&prompt=login&code_challenge={}&code_challenge_method=S256&client_id={CLIENT_ID}&scope=ReadOrigApi&nonce={}&state={}&redirect_uri={}",
            encode(&challenge(&self.verifier)),
            encode(&self.nonce),
            encode(&self.state),
            encode(REDIRECT),
        )
    }
}

/// A signed-in account, while someone chooses which dishwasher to add.
pub struct Account {
    client: reqwest::Client,
    token: String,
    region: String,
    pub dishwashers: Vec<Found>,
}

impl Account {
    /// Saves a dishwasher with its local key and feature map. `host` when
    /// its own name does not resolve. Returns its name.
    pub async fn add(&self, chosen: &Found, host: Option<&str>) -> Result<String> {
        let keys = encryption(&self.client, &self.region, &self.token, &chosen.ha_id).await?;
        let bytes = description(&self.client, &self.region, &self.token, &chosen.ha_id).await?;
        let features = xml::features_from_zip(&bytes)?;
        let host = host
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map_or_else(
                || discovered_host(chosen, keys.iv.is_empty()),
                str::to_owned,
            );
        let device = BoschDevice {
            aid: 0,
            door_aid: 0,
            leak_aid: 0,
            cycle_aid: 0,
            name: display_name(chosen),
            brand: pretty_brand(&chosen.brand),
            model: chosen.model.clone(),
            ha_id: chosen.ha_id.clone(),
            host,
            key: keys.key,
            iv: keys.iv,
            features,
        };
        let name = device.name.clone();
        let mut saved = config::load_or_new()?;
        config::upsert_bosch(&mut saved, device);
        config::save(&saved)?;
        Ok(name)
    }
}

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("could not start http")
}

async fn access_token(client: &reqwest::Client, pending: &SignIn, code: &str) -> Result<String> {
    let body = form(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code_verifier", &pending.verifier),
        ("code", code),
        ("redirect_uri", REDIRECT),
    ]);
    let response = client
        .post(TOKEN)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!("Home Connect refused that code. Run `hearth bosch login` again");
    }
    let value: Value = response.json().await?;
    value
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("Home Connect returned an unexpected token")
}

async fn appliances(
    client: &reqwest::Client,
    token: &str,
    account: &str,
) -> Result<(String, Value)> {
    let mut last = anyhow!("Home Connect did not return the account");
    for region in REGIONS {
        let url = format!("{region}/api/account/v2/accounts/{account}/paired-appliances");
        let response = client.get(url).bearer_auth(token).send().await?;
        if response.status().is_success() {
            return Ok(((*region).to_owned(), response.json().await?));
        }
        last = anyhow!("Home Connect returned {}", response.status());
    }
    Err(last)
}

async fn encryption(
    client: &reqwest::Client,
    region: &str,
    token: &str,
    ha_id: &str,
) -> Result<Keys> {
    let url = format!(
        "{region}/api/appliance/v2/appliances/{}/encryption-information",
        encode(ha_id)
    );
    let response = client.get(url).bearer_auth(token).send().await?;
    if !response.status().is_success() {
        anyhow::bail!(
            "Home Connect did not return a local key ({})",
            response.status()
        );
    }
    keys_from(&response.json().await?)
}

async fn description(
    client: &reqwest::Client,
    region: &str,
    token: &str,
    ha_id: &str,
) -> Result<Vec<u8>> {
    let url = format!("{region}/api/iddf/v1/iddf/{}", encode(ha_id));
    let response = client.get(url).bearer_auth(token).send().await?;
    if !response.status().is_success() {
        anyhow::bail!(
            "Home Connect did not return the appliance description ({})",
            response.status()
        );
    }
    let bytes = response.bytes().await?;
    if bytes.len() < 2 || &bytes[..2] != b"PK" {
        anyhow::bail!("Home Connect did not return the appliance description");
    }
    Ok(bytes.to_vec())
}

struct Keys {
    key: String,
    iv: String,
}

fn keys_from(body: &Value) -> Result<Keys> {
    keys_in(body)
        .or_else(|| body.get("data").and_then(keys_in))
        .context("Home Connect did not return a local key")
}

fn keys_in(body: &Value) -> Option<Keys> {
    if let Some(key) = body.get("tls").and_then(|tls| text(tls, &["key"])) {
        return Some(Keys {
            key,
            iv: String::new(),
        });
    }
    let aes = body.get("aes")?;
    let key = text(aes, &["key"])?;
    Some(Keys {
        key,
        iv: text(aes, &["iv"]).unwrap_or_default(),
    })
}

#[derive(Clone)]
pub struct Found {
    pub ha_id: String,
    brand: String,
    kind: String,
    model: String,
}

impl Found {
    /// How the menu bar lists it.
    pub fn label(&self) -> String {
        if self.model.is_empty() {
            display_name(self)
        } else {
            format!("{} ({})", display_name(self), self.model)
        }
    }
}

fn parse_appliances(body: &Value) -> Vec<Found> {
    let Some(items) = body
        .get("appliances")
        .or_else(|| body.get("data").and_then(|data| data.get("appliances")))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    items.iter().filter_map(found_from).collect()
}

fn found_from(value: &Value) -> Option<Found> {
    let ha_id = text(value, &["haId", "haid", "identifier", "id"])?;
    Some(Found {
        ha_id,
        brand: text(value, &["brand", "haBrand"]).unwrap_or_else(|| "BOSCH".into()),
        kind: text(value, &["haType", "type", "applianceType"]).unwrap_or_default(),
        model: text(value, &["vib", "eNumber", "enumber", "model"]).unwrap_or_default(),
    })
}

fn candidates(found: Vec<Found>, id: Option<&str>) -> Result<Vec<Found>> {
    if let Some(id) = id.map(str::trim).filter(|id| !id.is_empty()) {
        return found
            .into_iter()
            .find(|item| item.ha_id.eq_ignore_ascii_case(id))
            .map(|item| vec![item])
            .ok_or_else(|| anyhow!("no paired appliance has id {id}"));
    }
    // Only a dishwasher: anything else would be driven as one.
    let (dishwashers, others): (Vec<_>, Vec<_>) = found.into_iter().partition(is_dishwasher);
    if dishwashers.is_empty() {
        let others: Vec<String> = others
            .iter()
            .map(|item| format!("{} ({})", item.ha_id, item.kind))
            .collect();
        anyhow::bail!(
            "the Home Connect account has no dishwasher{}",
            if others.is_empty() {
                String::new()
            } else {
                format!(", only {}", others.join(", "))
            }
        );
    }
    Ok(dishwashers)
}

fn is_dishwasher(item: &Found) -> bool {
    let kind = item.kind.to_ascii_lowercase();
    let model = item.model.to_ascii_uppercase();
    kind.contains("dish")
        || model.starts_with("SPV")
        || model.starts_with("SMS")
        || model.starts_with("SMV")
}

fn discovered_host(item: &Found, tls: bool) -> String {
    if !tls {
        return item.ha_id.clone();
    }
    format!(
        "{}-{}-{}",
        host_part(&item.brand),
        host_part(&item.kind),
        host_part(&item.ha_id)
    )
}

fn host_part(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .collect()
}

fn display_name(item: &Found) -> String {
    let brand = pretty_brand(&item.brand);
    if is_dishwasher(item) {
        format!("{brand} Dishwasher")
    } else if item.kind.is_empty() {
        brand
    } else {
        format!("{brand} {}", label_of(&item.kind))
    }
}

fn pretty_brand(brand: &str) -> String {
    let brand = brand.trim();
    if brand.is_empty() {
        return "Bosch".into();
    }
    if brand
        .chars()
        .all(|ch| !ch.is_alphabetic() || ch.is_ascii_uppercase())
    {
        let mut chars = brand.chars();
        let Some(first) = chars.next() else {
            return "Bosch".into();
        };
        let mut out = first.to_ascii_uppercase().to_string();
        out.extend(chars.flat_map(|ch| ch.to_lowercase()));
        return out;
    }
    brand.to_owned()
}

fn text(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

fn subject(token: &str) -> Result<String> {
    let payload = token
        .split('.')
        .nth(1)
        .context("Home Connect returned an unexpected token")?;
    let bytes = b64_decode(payload)?;
    let value: Value =
        serde_json::from_slice(&bytes).context("Home Connect returned an unexpected token")?;
    value
        .get("sub")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("Home Connect returned an unexpected token")
}

pub fn parse_redirect(text: &str) -> Result<(String, Option<String>)> {
    let text = text.trim();
    if text.is_empty() {
        anyhow::bail!("that was empty");
    }
    let query = text.split_once('?').map(|(_, query)| query).unwrap_or(text);
    if !query.contains('=') {
        return Ok((percent_decode(text), None));
    }
    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "code" => code = Some(percent_decode(value)),
            "state" => state = Some(percent_decode(value)),
            _ => {}
        }
    }
    code.map(|code| (code, state))
        .context("that address has no code. Paste the whole hcauth address")
}

fn random_b64(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn b64_decode(text: &str) -> Result<Vec<u8>> {
    let mut padded = text.trim().to_owned();
    while !padded.len().is_multiple_of(4) {
        padded.push('=');
    }
    URL_SAFE
        .decode(padded)
        .context("Home Connect returned an unexpected token")
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sign_in_address_carries_the_challenge_not_the_verifier() {
        let pending = SignIn::new();
        let url = pending.address();
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=hcauth%3A%2F%2Fauth%2Fprod"));
        assert!(!url.contains(&pending.verifier));
    }

    #[test]
    fn a_redirect_keeps_the_code_and_the_state() {
        let (code, state) = parse_redirect("hcauth://auth/prod?code=abc%2Fdef&state=xyz").unwrap();
        assert_eq!(code, "abc/def");
        assert_eq!(state.as_deref(), Some("xyz"));
        let (code, state) = parse_redirect("just-the-code").unwrap();
        assert_eq!(code, "just-the-code");
        assert!(state.is_none());
    }

    #[test]
    fn the_account_id_comes_from_the_token_payload() {
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"sub":"user-1"}"#);
        let token = format!("aaa.{payload}.bbb");
        assert_eq!(subject(&token).unwrap(), "user-1");
    }

    #[test]
    fn one_dishwasher_is_chosen_from_a_mixed_account() {
        let body = serde_json::json!({
            "appliances": [
                {"haId": "OVEN-1", "brand": "BOSCH", "haType": "Oven", "vib": "HBA5780S0"},
                {"haId": "DW-1", "brand": "BOSCH", "haType": "Dishwasher", "vib": "SPV2HKX42G"}
            ]
        });
        let chosen = candidates(parse_appliances(&body), None).unwrap();
        assert_eq!(chosen.len(), 1);
        let chosen = &chosen[0];
        assert_eq!(chosen.ha_id, "DW-1");
        assert_eq!(display_name(chosen), "Bosch Dishwasher");
        assert_eq!(discovered_host(chosen, true), "BOSCH-Dishwasher-DW-1");
        let keys = keys_from(&serde_json::json!({"tls": {"key": "abc"}})).unwrap();
        assert!(keys.iv.is_empty());
        let keys = keys_from(&serde_json::json!({"aes": {"key": "abc", "iv": "def"}})).unwrap();
        assert_eq!(keys.iv, "def");
    }

    #[test]
    fn two_dishwashers_need_an_id() {
        let body = serde_json::json!({
            "appliances": [
                {"haId": "A", "brand": "BOSCH", "type": "Dishwasher", "vib": "SPV2HKX42G"},
                {"haId": "B", "brand": "BOSCH", "type": "Dishwasher", "vib": "SMS46IW01G"}
            ]
        });
        let found = parse_appliances(&body);
        assert_eq!(candidates(found.clone(), None).unwrap().len(), 2);
        let chosen = candidates(found, Some("b")).unwrap();
        assert_eq!(chosen[0].model, "SMS46IW01G");
    }

    #[test]
    fn an_account_without_a_dishwasher_is_refused() {
        let oven = Found {
            ha_id: "OVEN-1".into(),
            brand: "BOSCH".into(),
            kind: "Oven".into(),
            model: "XY01".into(),
        };
        let error = candidates(vec![oven], None).err().unwrap().to_string();
        assert!(error.contains("no dishwasher") && error.contains("OVEN-1"));
    }
}
