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
//! {"op":"rename","device":"tv","name":"Living room TV"}
//!   → {"ok":true}, and Home shows the new name
//! ```
//!
//! On a computer that is not serving Home, device requests go on to the one
//! that is (see `cluster`), and the status is its devices. The status also
//! has a `group`, and these ops act on it:
//!
//! ```text
//! {"op":"approve","code":"123 456"}  let in the computer asking to join
//! {"op":"decline"}                   turn it away
//! {"op":"confirm"}                   this computer's code was typed there
//! {"op":"separate"}                  stop asking; keep a group of our own
//! {"op":"join"}                      leave ours for the other group heard
//! ```
//!
//! The socket is mode 0600 and never on the network, so only this user can
//! control the devices. State is served from memory, so a reply is immediate.

use crate::{capabilities, cluster, config};
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
    Rename {
        device: String,
        name: String,
    },
    Approve {
        code: String,
    },
    Decline,
    Confirm,
    Separate,
    Join,
}

pub fn path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("control.sock"))
}

/// Serves the socket for as long as the bridge runs.
pub async fn serve() -> Result<()> {
    let path = path()?;
    fs::create_dir_all(config::config_dir()?)?;
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
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return json!({ "error": "bad request: not JSON" });
    };
    let request = match Request::deserialize(&value) {
        Ok(request) => request,
        Err(error) => return json!({ "error": format!("bad request: {error}") }),
    };
    match request {
        Request::Status => {
            let mut status = match cluster::forward(&value).await {
                Some(Ok(status)) => status,
                _ => here(value).await,
            };
            if status.get("devices").is_none() {
                status = idle();
            }
            status["pid"] = json!(std::process::id());
            status["group"] = cluster::status();
            status
        }
        Request::Approve { code } => done(cluster::approve(&code)),
        Request::Decline => done(cluster::decline()),
        Request::Confirm => done(cluster::confirm()),
        Request::Separate => done(cluster::separate()),
        Request::Join => done(cluster::join_other()),
        _ => match cluster::forward(&value).await {
            Some(reply) => reply.unwrap_or_else(|error| json!({ "error": format!("{error:#}") })),
            None => here(value).await,
        },
    }
}

/// Answers a device request from this computer's own devices. For the
/// computer serving Home; requests from other members end here.
pub async fn here(value: Value) -> Value {
    match Request::deserialize(&value) {
        Ok(Request::Status) => capabilities::status().unwrap_or_else(idle),
        Ok(Request::Get { device }) => capabilities::get(&device)
            .unwrap_or_else(|error| json!({ "error": format!("{error:#}") })),
        Ok(Request::Set {
            device,
            capability,
            value,
        }) => done(capabilities::set(&device, &capability, value).await),
        Ok(Request::Rename { device, name }) => done(capabilities::rename(&device, &name)),
        Ok(_) => json!({ "error": "not a device request" }),
        Err(error) => json!({ "error": format!("bad request: {error}") }),
    }
}

/// The status while no computer serves Home yet.
fn idle() -> Value {
    let config = config::load_or_new().unwrap_or_default();
    json!({
        "name": config.name,
        "pin": config.homekit_pin,
        "paired": config::paired(),
        "devices": [],
    })
}

fn done(result: Result<()>) -> Value {
    match result {
        Ok(()) => json!({ "ok": true }),
        Err(error) => json!({ "error": format!("{error:#}") }),
    }
}

/// Whether a bridge is answering on the socket.
pub async fn running() -> bool {
    match path() {
        Ok(path) => UnixStream::connect(path).await.is_ok(),
        Err(_) => false,
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
