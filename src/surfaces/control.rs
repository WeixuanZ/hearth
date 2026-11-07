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
//! {"op":"products"}
//!   → the devices that can be added
//! {"op":"setup","product":"samsung-frame"}  then  {"op":"setup","product":"samsung-frame","answers":{…}}
//!   → {"step":{title, note, fields, link, wait}}, until {"done":"Added …"}
//! {"op":"history","device":"purifier","capability":"temperature","hours":24}
//!   → readings for that long, one point a minute at most
//! {"op":"normals","device":"purifier","capability":"temperature"}
//!   → its usual day from the last 30, an hour at a time, and today so far
//! {"op":"subscribe","topic":"hearth/bosch:"}
//!   → every device as it is, then each event as it happens, a line each:
//!     {"event":"changed","topic":"hearth/<device>/<capability>",…}, or
//!     device, raised, cleared. topic, a prefix, is optional
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

use crate::{
    bus,
    capabilities::{self, Capability, Kind},
    cluster, config, series, setup,
};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::PathBuf};
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
    History {
        device: String,
        #[serde(default)]
        capability: Option<String>,
        #[serde(default)]
        hours: Option<u64>,
    },
    Normals {
        device: String,
        capability: String,
    },
    /// Events as they happen, on this connection, until it closes.
    Subscribe {
        #[serde(default)]
        topic: Option<String>,
    },
    Products,
    Setup {
        product: String,
        #[serde(default)]
        answers: Option<setup::Answers>,
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
        if let Ok(Request::Subscribe { topic }) = serde_json::from_str(&line) {
            return stream_events(&mut write, topic.unwrap_or_default()).await;
        }
        let mut reply = answer(&line).await.to_string();
        reply.push('\n');
        write.write_all(reply.as_bytes()).await?;
    }
    Ok(())
}

/// Every device as it now is, then each change as it happens, a JSON line
/// each, until the other side hangs up. Only topics starting with `topic`.
/// Devices are on the computer serving Home; elsewhere the stream is quiet.
async fn stream_events(write: &mut tokio::net::unix::OwnedWriteHalf, topic: String) -> Result<()> {
    let mut state = bus::watch();
    let mut before = bus::Devices::new();
    loop {
        let now = state.borrow_and_update().clone();
        for change in bus::changes(&before, &now) {
            if change.topic().starts_with(&topic) {
                let mut line = serde_json::to_string(&change)?;
                line.push('\n');
                write.write_all(line.as_bytes()).await?;
            }
        }
        before = (*now).clone();
        if state.changed().await.is_err() {
            return Ok(());
        }
    }
}

/// Events from the running bridge, as they happen. Dropping it ends the
/// subscription.
pub struct Subscription {
    lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    /// Held open: the bridge ends the stream when this side closes.
    _write: tokio::net::unix::OwnedWriteHalf,
}

impl Subscription {
    /// Events whose topic starts with `topic`; every event for "".
    pub async fn open(topic: &str) -> Result<Self> {
        let stream = UnixStream::connect(path()?).await.map_err(|_| {
            anyhow!("the bridge is not running. Start it with `hearth run` or the menu bar")
        })?;
        let (read, mut write) = stream.into_split();
        let body = json!({ "op": "subscribe", "topic": topic });
        write.write_all(format!("{body}\n").as_bytes()).await?;
        Ok(Self {
            lines: BufReader::new(read).lines(),
            _write: write,
        })
    }

    /// The next event, or none once the bridge stops.
    pub async fn next(&mut self) -> Result<Option<Value>> {
        match self.lines.next_line().await? {
            Some(line) => Ok(Some(serde_json::from_str(&line)?)),
            None => Ok(None),
        }
    }
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
            let status = match cluster::forward(&value).await {
                Some(Ok(status)) => status,
                Some(Err(error)) => {
                    log::debug!("status: {error:#}");
                    Value::Null
                }
                None => here(value).await,
            };
            let mut status = recall(status, crate::clock::now_ms());
            status["pid"] = json!(std::process::id());
            status["group"] = cluster::status();
            status
        }
        // On this computer: the device joins the group's config from here.
        Request::Products => json!(setup::products()),
        Request::Setup { product, answers } => setup::step(&product, answers)
            .await
            .unwrap_or_else(|error| json!({ "error": format!("{error:#}") })),
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
        Ok(Request::History {
            device,
            capability,
            hours,
        }) => history(&device, capability.as_deref(), hours.unwrap_or(24))
            .unwrap_or_else(|error| json!({ "error": format!("{error:#}") })),
        Ok(Request::Normals { device, capability }) => normals(&device, &capability)
            .unwrap_or_else(|error| json!({ "error": format!("{error:#}") })),
        Ok(_) => json!({ "error": "not a device request" }),
        Err(error) => json!({ "error": format!("bad request: {error}") }),
    }
}

/// How long the devices last seen stand in for ones not heard from.
const RECALL_MS: u64 = 30_000;

/// What the menu bar was last shown, and when.
struct Seen {
    /// The last status from the computer serving Home.
    base: Option<(u64, Value)>,
    /// Each device as last reported, by id.
    devices: BTreeMap<String, (u64, Value)>,
}

static SEEN: std::sync::Mutex<Seen> = std::sync::Mutex::new(Seen {
    base: None,
    devices: BTreeMap::new(),
});

/// A status with every device in it. While Home moves to another computer,
/// or the one serving it is slow to answer, or a device has not reported
/// since serving began, the device stays as it was last seen, for a while,
/// rather than vanishing from the menu bar.
fn recall(status: Value, now: u64) -> Value {
    let wanted: Vec<String> = config::load()
        .map(|config| config.devices.iter().map(|device| device.id()).collect())
        .unwrap_or_default();
    let mut seen = SEEN.lock().unwrap_or_else(|poison| poison.into_inner());
    fill(status, &mut seen, &wanted, now).unwrap_or_else(idle)
}

/// `status`, with what `seen` remembers of the `wanted` devices it lacks.
/// Nothing if there is no status, nor a recent one to stand in.
fn fill(status: Value, seen: &mut Seen, wanted: &[String], now: u64) -> Option<Value> {
    let recent = |when: u64| now.saturating_sub(when) < RECALL_MS;
    let id = |device: &Value| device["id"].as_str().map(str::to_owned);
    let answered = status["devices"].is_array();
    let mut status = if answered {
        seen.base = Some((now, status.clone()));
        status
    } else {
        seen.base
            .as_ref()
            .filter(|(when, _)| recent(*when))?
            .1
            .clone()
    };
    let devices = status["devices"].as_array_mut()?;
    if answered {
        for device in devices.iter() {
            if let Some(id) = id(device) {
                seen.devices.insert(id, (now, device.clone()));
            }
        }
    } else {
        devices.clear();
    }
    seen.devices.retain(|id, _| wanted.contains(id));
    let have: Vec<String> = devices.iter().filter_map(id).collect();
    for id in wanted.iter().filter(|id| !have.contains(id)) {
        if let Some((_, device)) = seen.devices.get(id).filter(|(when, _)| recent(*when)) {
            devices.push(device.clone());
        }
    }
    Some(status)
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

/// A device's readings over the last `hours`, from this computer's record.
fn history(device: &str, capability: Option<&str>, hours: u64) -> Result<Value> {
    let snapshot = capabilities::snapshot(device)?;
    let readings: Vec<(&Capability, &str)> = snapshot
        .capabilities
        .iter()
        .filter(|reading| capability.is_none_or(|wanted| reading.id == wanted))
        .filter_map(|reading| match reading.kind {
            Kind::Reading { unit } => Some((reading, unit)),
            _ => None,
        })
        .collect();
    if let (Some(wanted), true) = (capability, readings.is_empty()) {
        if snapshot.capabilities.iter().any(|item| item.id == wanted) {
            anyhow::bail!("{wanted} is not a reading");
        }
        anyhow::bail!("{} has no capability {wanted}", snapshot.name);
    }
    let (hours, since) = series::window(hours, crate::clock::now());
    let samples: Vec<_> = readings
        .iter()
        .map(|(reading, _)| series::samples(&snapshot.id, reading.id, since))
        .collect();
    let views: Vec<_> = readings
        .iter()
        .zip(&samples)
        .map(|((reading, unit), samples)| series::SeriesOut {
            capability: reading.id,
            name: &reading.name,
            unit,
            samples,
        })
        .collect();
    let path = series::csv_path(&snapshot.id)?;
    Ok(series::document(
        &snapshot.id,
        &snapshot.name,
        &path,
        hours,
        since,
        &views,
    ))
}

/// A reading's usual day, from the last thirty, and today so far beside it.
fn normals(device: &str, capability: &str) -> Result<Value> {
    use chrono::{Local, TimeZone, Timelike};
    let snapshot = capabilities::snapshot(device)?;
    let unit = snapshot
        .capabilities
        .iter()
        .find_map(|reading| match reading.kind {
            Kind::Reading { unit } if reading.id == capability => Some(unit),
            _ => None,
        })
        .ok_or_else(|| anyhow!("{} has no reading {capability}", snapshot.name))?;
    let now = crate::clock::now();
    let today = Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| Local.from_local_datetime(&midnight).earliest())
        .map_or(now - now % 86_400, |midnight| {
            midnight.timestamp().max(0) as u64
        });
    let samples = series::samples(&snapshot.id, capability, series::window(720, now).1);
    let hour_of = |time: u64| {
        Local
            .timestamp_opt(time as i64, 0)
            .single()
            .map_or(0, |moment| moment.hour())
    };
    let (normals, days) = series::normals(&samples, today, hour_of);
    let today: Vec<_> = samples
        .iter()
        .filter(|(time, _)| *time >= today)
        .map(|&(time, milli)| json!({ "t": time, "v": f64::from(milli) / 1000.0 }))
        .collect();
    Ok(json!({
        "device": snapshot.id,
        "capability": capability,
        "unit": unit,
        "days": days,
        "normals": normals,
        "today": today,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{Capability, Snapshot};

    #[test]
    fn a_device_not_heard_from_stays_as_last_seen_for_a_while() {
        let mut seen = Seen {
            base: None,
            devices: BTreeMap::new(),
        };
        let wanted = ["tv".to_owned(), "lamp".to_owned()];
        let ids = |status: &Value| -> Vec<String> {
            status["devices"]
                .as_array()
                .unwrap()
                .iter()
                .map(|device| device["id"].as_str().unwrap().to_owned())
                .collect()
        };
        let both = json!({ "devices": [{ "id": "tv" }, { "id": "lamp" }] });
        assert_eq!(
            ids(&fill(both, &mut seen, &wanted, 0).unwrap()),
            ["tv", "lamp"]
        );
        // Serving just began elsewhere: the lamp has not reported yet.
        let tv = json!({ "devices": [{ "id": "tv" }] });
        let status = fill(tv.clone(), &mut seen, &wanted, 5_000).unwrap();
        assert_eq!(ids(&status), ["tv", "lamp"]);
        // The computer serving Home did not answer.
        let status = fill(Value::Null, &mut seen, &wanted, 10_000).unwrap();
        assert_eq!(ids(&status), ["tv", "lamp"]);
        // Long enough, and only what is heard from shows.
        let status = fill(tv, &mut seen, &wanted, 5_000 + RECALL_MS).unwrap();
        assert_eq!(ids(&status), ["tv"]);
        assert!(fill(Value::Null, &mut seen, &wanted, 10 * RECALL_MS).is_none());
        // A removed device is not brought back.
        let status = fill(
            json!({ "devices": [] }),
            &mut seen,
            &["lamp".into()],
            5_000 + RECALL_MS,
        )
        .unwrap();
        assert_eq!(ids(&status), Vec::<String>::new());
    }

    fn device(on: bool) -> Snapshot {
        Snapshot {
            id: "bus-test:1".into(),
            kind: "test",
            category: "purifier",
            name: "Fan".into(),
            online: true,
            headline: String::new(),
            detail: String::new(),
            capabilities: vec![Capability::power(on)],
            alerts: Vec::new(),
            about: Default::default(),
        }
    }

    async fn next(
        lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    ) -> Value {
        let line = lines.next_line().await.unwrap().unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn a_subscriber_hears_the_device_then_each_change() {
        capabilities::begin(&config::Config::default());
        let (client, server) = UnixStream::pair().unwrap();
        tokio::spawn(connection(server));
        let (read, mut write) = client.into_split();
        let mut lines = BufReader::new(read).lines();
        write
            .write_all(b"{\"op\":\"subscribe\",\"topic\":\"hearth/bus-test:1/\"}\n")
            .await
            .unwrap();
        capabilities::publish(device(false));
        // Whether it came in the first look or as it happened, it is whole.
        let first = next(&mut lines).await;
        assert_eq!(first["event"], "device");
        assert_eq!(first["device"]["id"], "bus-test:1");

        capabilities::publish(device(true));
        // Read on to the change: the device as it now is, then what changed.
        let mut before = first;
        let changed = loop {
            let event = next(&mut lines).await;
            if event["event"] == "changed" && event["value"] == true {
                break event;
            }
            before = event;
        };
        assert_eq!(changed["topic"], "hearth/bus-test:1/power");
        assert_eq!(before["event"], "device");
        assert_eq!(before["device"]["capabilities"][0]["value"], true);
    }
}
