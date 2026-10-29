//! Samsung IP Control: JSON-RPC over HTTPS on port 1516, on the LAN.
//!
//! It is the one local way to pick a specific input, power fully off, and read
//! the current input. Turn it on in Settings → General → Network → Expert
//! Settings → IP Remote, then pair while the TV shows a picture, not art.
//! Inputs only switch to sockets with something plugged in.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub struct IpControl {
    client: reqwest::Client,
    url: String,
    token: String,
}

impl IpControl {
    pub fn new(host: &str, token: &str) -> Result<Self> {
        Ok(Self {
            client: client(Duration::from_secs(5))?,
            url: format!("https://{host}:1516/"),
            token: token.to_owned(),
        })
    }

    /// Asks the TV for a token. The TV shows Allow, and waits for it.
    pub async fn pair(host: &str) -> Result<String> {
        let reply = post(
            &client(Duration::from_secs(90))?,
            &format!("https://{host}:1516/"),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "createAccessToken" }),
        )
        .await?;
        reply["AccessToken"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("the TV did not hand back an IP Control token"))
    }

    pub async fn set_power(&self, on: bool) -> Result<()> {
        let power = if on { "powerOn" } else { "powerOff" };
        self.call("powerControl", json!({ "power": power })).await?;
        Ok(())
    }

    /// The current source, like `HDMI4` or `TV`.
    pub async fn input(&self) -> Result<String> {
        let reply = self.call("inputSourceControl", json!({})).await?;
        reply["inputSource"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("the TV did not name its input"))
    }

    pub async fn set_input(&self, source: &str) -> Result<()> {
        self.call("inputSourceControl", json!({ "inputSource": source }))
            .await
            .with_context(|| {
                format!("the TV would not switch to {source}. Is anything plugged in?")
            })?;
        Ok(())
    }

    async fn call(&self, method: &str, mut params: Value) -> Result<Value> {
        params["AccessToken"] = json!(self.token);
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        post(&self.client, &self.url, body).await
    }
}

/// The TV's certificate is self-signed. Its server answers 400 to lowercase
/// header names and to `Accept: */*`, so both are spelled out.
fn client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .http1_title_case_headers()
        .timeout(timeout)
        .build()?)
}

async fn post(client: &reqwest::Client, url: &str, body: Value) -> Result<Value> {
    let response = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&body)
        .send()
        .await
        .context("IP Control did not answer. Is IP Remote on?")?;
    let status = response.status();
    let text = response.text().await?;
    let reply: Value = serde_json::from_str(&text)
        .with_context(|| format!("IP Control answered {status}: {text:?}"))?;
    if let Some(error) = reply.get("error") {
        return Err(anyhow!("IP Control refused: {error}"));
    }
    Ok(reply["result"].clone())
}
