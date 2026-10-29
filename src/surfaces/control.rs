//! The local control socket, `~/.config/hearth/control.sock`.
//!
//! The menu bar and the CLI talk to a running bridge through it. One JSON
//! request per line, one JSON reply per line:
//!
//! ```text
//! {"op":"status"}
//!   → {"pid":…,"name":…,"pin":…,"paired":…,"devices":[…]}
//! {"op":"get","device":"tv"}
//!   → one device, with its capabilities
//! {"op":"set","device":"tv","capability":"input","value":"hdmi2"}
//!   → {"ok":true}  or  {"error":"…"}
//! ```
//!
//! The socket is mode 0600 and never on the network, so only this user can
//! control the devices. State is served from memory, so a reply is immediate.

use crate::{capabilities, config};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Status,
    Get {
        device: String,
    },
    Set {
        device: String,
        capability: String,
        value: Value,
    },
}

pub fn path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("control.sock"))
}

/// Serves the socket for as long as the bridge runs.
pub async fn serve() -> Result<()> {
    let path = path()?;
    let _ = fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).with_context(|| format!("could not open {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(error) = connection(stream).await {
                log::debug!("control: {error:#}");
            }
        });
    }
}

/// Removes the socket when dropped, so a stale one does not look like a
/// running bridge.
pub struct Socket;

impl Drop for Socket {
    fn drop(&mut self) {
        if let Ok(path) = path() {
            let _ = fs::remove_file(path);
        }
    }
}

async fn connection(stream: UnixStream) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let mut reply = answer(&line).await.to_string();
        reply.push('\n');
        write.write_all(reply.as_bytes()).await?;
    }
    Ok(())
}

async fn answer(line: &str) -> Value {
    match serde_json::from_str::<Request>(line) {
        Ok(Request::Status) => capabilities::status(),
        Ok(Request::Get { device }) => capabilities::get(&device)
            .unwrap_or_else(|error| json!({ "error": format!("{error:#}") })),
        Ok(Request::Set {
            device,
            capability,
            value,
        }) => match capabilities::set(&device, &capability, value).await {
            Ok(()) => json!({ "ok": true }),
            Err(error) => json!({ "error": format!("{error:#}") }),
        },
        Err(error) => json!({ "error": format!("bad request: {error}") }),
    }
}

/// Sends one request to the running bridge and returns its reply.
pub async fn request(body: Value) -> Result<Value> {
    let stream = UnixStream::connect(path()?).await.map_err(|_| {
        anyhow!("the bridge is not running. Start it with `hearth run` or the menu bar")
    })?;
    let (read, mut write) = stream.into_split();
    write.write_all(format!("{body}\n").as_bytes()).await?;
    let line = BufReader::new(read)
        .lines()
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("the bridge closed the connection"))?;
    let reply: Value = serde_json::from_str(&line)?;
    match reply.get("error").and_then(Value::as_str) {
        Some(error) => Err(anyhow!("{error}")),
        None => Ok(reply),
    }
}
