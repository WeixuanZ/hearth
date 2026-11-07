//! One HomeKit bridge for every computer in a group on this network.
//!
//! Computers find each other by a hello on UDP. One without a group asks a
//! member to let it in, and a person approves it with a code (see `crypto`).
//! Members then trust each other's signed hellos and sealed requests. They
//! share the config and the HomeKit pairing, and one of them serves Home:
//! whoever is serving keeps serving, and if it goes quiet the lowest id takes
//! over as the same bridge, so Home does not notice. The others pass the menu
//! bar's and the CLI's requests on to it.

mod crypto;
mod election;
mod files;
mod joining;
mod wire;

use election::*;
use files::*;
use joining::*;
pub use joining::{approve, confirm, decline, join_other, separate};

use crate::{
    capabilities,
    config::{self, Config},
    run::Serving,
    series,
    surfaces::control,
};
use anyhow::{anyhow, Context, Result};
use crypto::{Ephemeral, GroupKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    net::{IpAddr, SocketAddr},
    path::{Component, Path},
    sync::Mutex,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{oneshot, watch},
    time::timeout,
};
use wire::{Hello, Message, Request, State};

/// Where hellos go. Every computer takes TCP on a port its hello names.
const PORT: u16 = 47_620;
const TICK: Duration = Duration::from_secs(1);
/// A computer not heard from for this long has gone.
const FRESH_MS: u64 = 4_000;
/// How long a new process listens before it serves, so two computers that
/// start together do not both serve.
const GRACE_MS: u64 = 1_500;
/// How long a computer listens for a group before it starts its own, and how
/// long when a lower id is looking too, which should start it instead.
const FOUND_MS: u64 = 3_000;
const LOWER_MS: u64 = 15_000;
/// How long someone has to approve a computer that asks to join.
const ASKING: Duration = Duration::from_secs(180);
/// How long a member has to answer a request. Long enough for a TV to wake
/// for a forwarded request; short enough that one stalled member cannot hold
/// up the hellos and a takeover.
const ANSWER: Duration = Duration::from_secs(15);

struct Group {
    id: String,
    name: String,
    key: Option<GroupKey>,
    role: Role,
    /// The member serving Home, while this one is on standby.
    leader: Option<SocketAddr>,
    members: Vec<String>,
    /// A member paired with another Home, as a different bridge.
    conflict: bool,
    /// Another group on the network: its id, and a member's name.
    other: Option<(String, String)>,
    /// A computer asking to join this one.
    asking: Option<Asking>,
    /// This computer asking to join another.
    joining: Option<Joining>,
    /// Ask to join this group, after leaving ours for it.
    target: Option<String>,
    /// Do not ask to join: a person chose to, or a member said no.
    separate: bool,
    /// The network refused this computer's hello, as macOS does to an app
    /// it has not allowed on the local network. Devices cannot be reached
    /// either, so the menu bar says so.
    blocked: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Role {
    Looking,
    Serving,
    Standby { leader: String },
}

static GROUP: Mutex<Option<Group>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Group) -> T) -> Result<T> {
    let mut guard = GROUP.lock().unwrap_or_else(|poison| poison.into_inner());
    guard
        .as_mut()
        .map(f)
        .ok_or_else(|| anyhow!("the bridge is starting"))
}

struct Heard {
    hello: Hello,
    addr: IpAddr,
    seen: u64,
    /// In our group, with a hello signed by our key.
    member: bool,
}

pub async fn run() -> Result<()> {
    let mut config = config::load_or_new()?;
    if !config::config_path()?.exists() {
        config::save(&config)?;
    }
    let udp = udp()?;
    let tcp = TcpListener::bind(("0.0.0.0", 0)).await?;
    let port = tcp.local_addr()?.port();
    *GROUP.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(Group {
        id: node_id()?,
        name: computer_name(),
        key: GroupKey::load(),
        role: Role::Looking,
        leader: None,
        members: Vec::new(),
        conflict: false,
        other: None,
        asking: None,
        joining: None,
        target: None,
        separate: false,
        blocked: false,
    });
    tokio::spawn(accept(tcp));

    let started = crate::clock::now_ms();
    let mut heard = BTreeMap::new();
    let mut serving = None;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut buffer = [0u8; 4096];
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if let Err(error) = step(&udp, port, started, &mut config, &mut heard, &mut serving).await {
                    log::warn!("group: {error:#}");
                }
            }
            received = udp.recv_from(&mut buffer) => {
                let (length, from) = received?;
                hear(&buffer[..length], from.ip(), &mut heard);
            }
        }
    }
}

/// Once a second: say hello, catch up with the members, and serve Home or not.
async fn step(
    udp: &UdpSocket,
    port: u16,
    started: u64,
    config: &mut Config,
    heard: &mut BTreeMap<String, Heard>,
    serving: &mut Option<Serving>,
) -> Result<()> {
    let now = crate::clock::now_ms();
    heard.retain(|_, peer| now.saturating_sub(peer.seen) < 2 * FRESH_MS);
    *config = config::load()?;
    series::forget_removed(config.removed.keys());
    let files = Files::read()?;
    let (id, name, key) = with(|group| (group.id.clone(), group.name.clone(), group.key.clone()))?;

    let mut hello = Hello {
        id: id.clone(),
        name,
        port,
        group: key.as_ref().map(|key| key.id.clone()).unwrap_or_default(),
        bridge: config.bridge_id.clone(),
        paired: config::paired(),
        revision: config::revision(config),
        storage: files.hash.clone(),
        serving: serving.is_some(),
        sent: now,
        tag: String::new(),
    };
    if let Some(key) = &key {
        hello.sign(&key.hello());
    }
    // A refused hello does not stop this computer serving: alone, it is
    // still the one to serve, and its devices show offline, not gone.
    let sent = udp
        .send_to(&serde_json::to_vec(&hello)?, ("255.255.255.255", PORT))
        .await;
    let blocked = sent.is_err();
    let was = with(|group| std::mem::replace(&mut group.blocked, blocked))?;
    match (&sent, was) {
        (Err(error), false) => log::warn!(
            "group: the network refused this computer ({error}). On a Mac, allow Hearth in System Settings › Privacy & Security › Local Network"
        ),
        (Ok(_), true) => log::info!("group: the network is reachable again"),
        _ => {}
    }

    // Another group to move to. Without a group, this one is already asking.
    let other = heard
        .values()
        .filter(|_| key.is_some())
        .find(|peer| !peer.member && !peer.hello.group.is_empty())
        .map(|peer| (peer.hello.group.clone(), peer.hello.name.clone()));
    with(|group| group.other = other)?;

    let Some(key) = key else {
        *serving = None;
        return look(&id, started, now, heard);
    };
    let fresh: Vec<&Heard> = heard
        .values()
        .filter(|peer| peer.member && now.saturating_sub(peer.seen) < FRESH_MS)
        .collect();
    for peer in &fresh {
        catch_up(peer, &key, config, &files, serving.is_some()).await;
    }
    if now % 60_000 < TICK.as_millis() as u64 {
        for peer in &fresh {
            if let Err(error) = share_history(peer, &key, config).await {
                log::debug!(
                    "could not share history with {}: {error:#}",
                    peer.hello.name
                );
            }
        }
    }

    let paired = config::paired();
    let conflict = fresh
        .iter()
        .any(|peer| paired && peer.hello.paired && peer.hello.bridge != config.bridge_id);
    // Only members serving the same bridge stand for it.
    let same: Vec<&Hello> = fresh
        .iter()
        .filter(|peer| peer.hello.bridge == config.bridge_id)
        .map(|peer| &peer.hello)
        .collect();
    let role = role(&id, serving.is_some(), &same, now, started);
    let leader = match &role {
        Role::Serving => {
            let layout = crate::run::Layout::of(config);
            if serving
                .as_ref()
                .is_none_or(|serving| serving.layout != layout || serving.stopped())
            {
                *serving = None;
                *serving = Some(Serving::start(config.clone())?);
            }
            None
        }
        Role::Standby { .. } => {
            *serving = None;
            fresh
                .iter()
                .find(|peer| peer.hello.serving && peer.hello.bridge == config.bridge_id)
                .map(|peer| SocketAddr::new(peer.addr, peer.hello.port))
        }
        Role::Looking => {
            *serving = None;
            None
        }
    };
    let members = fresh.iter().map(|peer| peer.hello.name.clone()).collect();
    with(|group| {
        group.role = role;
        group.leader = leader;
        group.members = members;
        group.conflict = conflict;
    })
}

/// Takes a member's config, and its HomeKit files if it serves Home, when
/// they are news to this computer.
async fn catch_up(peer: &Heard, key: &GroupKey, config: &mut Config, files: &Files, serving: bool) {
    let hello = &peer.hello;
    let paired = config::paired();
    let same = hello.bridge == config.bridge_id;
    let pairing = !serving && hello.serving && same && hello.storage != files.hash;
    // Two bridges that two Homes know stay apart. Otherwise they become one.
    let two_homes = paired && hello.paired;
    let wanted = hello.revision > config::revision(config) || !(same || two_homes) || pairing;
    if !wanted {
        return;
    }
    let addr = SocketAddr::new(peer.addr, hello.port);
    let result = async {
        let state: State = serde_json::from_value(ask(addr, key, &Request::State).await?)?;
        let merged = config::merge(config, &state.config, paired, state.paired);
        if merged.config != *config {
            config::write(&merged.config)?;
            *config = merged.config;
            for device in &config.devices {
                // A rename made elsewhere, shown now. Nothing if not serving.
                let _ = capabilities::rename(&device.id(), device.name());
            }
        }
        if pairing && state.config.bridge_id == config.bridge_id {
            Files::install(&state.files)?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        log::debug!("could not catch up with {}: {error:#}", hello.name);
    }
}

/// Takes the days of readings history a member holds differently, so every
/// member keeps the whole record and any can take over with it. Each pulls
/// from each, once a minute; merges converge, see `series::merge`.
async fn share_history(peer: &Heard, key: &GroupKey, config: &Config) -> Result<()> {
    let addr = SocketAddr::new(peer.addr, peer.hello.port);
    let theirs = series::their_digest(ask(addr, key, &Request::HistoryDigest).await?)?;
    let ours = series::their_digest(series::digest())?;
    for (device, days) in theirs {
        if config.removed.contains_key(&device) {
            continue;
        }
        let mine = ours.get(&device);
        let days: Vec<u64> = days
            .into_iter()
            .filter(|(day, hash)| mine.and_then(|mine| mine.get(day)) != Some(hash))
            .map(|(day, _)| day)
            .collect();
        if days.is_empty() {
            continue;
        }
        let request = Request::HistoryRows {
            device: device.clone(),
            days,
        };
        series::merge(
            &device,
            serde_json::from_value(ask(addr, key, &request).await?)?,
        );
    }
    Ok(())
}

fn hear(bytes: &[u8], addr: IpAddr, heard: &mut BTreeMap<String, Heard>) {
    let Ok(hello) = serde_json::from_slice::<Hello>(bytes) else {
        return;
    };
    let now = crate::clock::now_ms();
    let Ok((me, key)) = with(|group| (group.id.clone(), group.key.clone())) else {
        return;
    };
    if hello.id == me || hello.sent.abs_diff(now) > 30_000 {
        return;
    }
    let ours = key.as_ref().is_some_and(|key| key.id == hello.group);
    let member = ours && key.is_some_and(|key| hello.verify(&key.hello()));
    // Our group's id, but not our key: someone pretending.
    if ours && !member {
        return;
    }
    heard.insert(
        hello.id.clone(),
        Heard {
            hello,
            addr,
            seen: now,
            member,
        },
    );
}

/// Sends a request to a member and returns its reply.
async fn ask(addr: SocketAddr, key: &GroupKey, request: &Request) -> Result<Value> {
    let mut stream = timeout(Duration::from_secs(3), TcpStream::connect(addr))
        .await
        .context("the other computer did not answer")??;
    timeout(ANSWER, exchange(&mut stream, key, request))
        .await
        .context("the other computer stopped answering")?
}

async fn exchange(stream: &mut TcpStream, key: &GroupKey, request: &Request) -> Result<Value> {
    let Message::Nonce { nonce } = wire::receive(stream).await? else {
        anyhow::bail!("the other computer did not start properly");
    };
    let nonce = crypto::decode(&nonce)?;
    let sealed = crypto::seal(&key.message(), &nonce, &serde_json::to_vec(request)?);
    let message = Message::Sealed {
        sealed: crypto::encode(&sealed),
    };
    wire::send(stream, &message).await?;
    let Message::Sealed { sealed } = wire::receive(stream).await? else {
        anyhow::bail!("the other computer refused");
    };
    let reply = crypto::open(
        &key.message(),
        &reply_context(&nonce),
        &crypto::decode(&sealed)?,
    )?;
    Ok(serde_json::from_slice(&reply)?)
}

fn reply_context(nonce: &[u8]) -> Vec<u8> {
    [nonce, b"reply"].concat()
}

async fn accept(listener: TcpListener) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(async move {
            if let Err(error) = answer(stream).await {
                log::debug!("group: {error:#}");
            }
        });
    }
}

async fn answer(mut stream: TcpStream) -> Result<()> {
    let nonce: [u8; 16] = rand::random();
    let message = Message::Nonce {
        nonce: crypto::encode(&nonce),
    };
    wire::send(&mut stream, &message).await?;
    match timeout(Duration::from_secs(10), wire::receive(&mut stream)).await?? {
        Message::Join { name, public, .. } => admit(stream, name, &public).await,
        Message::Sealed { sealed } => {
            let key = with(|group| group.key.clone())?.context("not in a group")?;
            let request = crypto::open(&key.message(), &nonce, &crypto::decode(&sealed)?)?;
            let reply = match serde_json::from_slice(&request)? {
                Request::State => serde_json::to_value(State {
                    config: config::load()?,
                    paired: config::paired(),
                    files: Files::read()?.encoded(),
                })?,
                Request::Control { request } => control::here(request).await,
                Request::HistoryDigest => series::digest(),
                Request::HistoryRows { device, days } => json!(series::rows(&device, &days)),
            };
            let sealed = crypto::seal(
                &key.message(),
                &reply_context(&nonce),
                &serde_json::to_vec(&reply)?,
            );
            let message = Message::Sealed {
                sealed: crypto::encode(&sealed),
            };
            wire::send(&mut stream, &message).await
        }
        _ => Ok(()),
    }
}

/// The `group` part of the control socket's status.
pub fn status() -> Value {
    with(|group| {
        let (role, leader) = match &group.role {
            Role::Serving if group.conflict => ("conflict", None),
            Role::Serving => ("serving", None),
            Role::Standby { leader } => ("standby", Some(leader.clone())),
            Role::Looking if group.joining.is_some() => ("joining", None),
            Role::Looking => ("looking", None),
        };
        let joining = group.joining.as_ref();
        json!({
            "name": group.name,
            "role": role,
            "leader": leader.or_else(|| joining.map(|joining| joining.host.clone())),
            "members": group.members,
            "asking": group.asking.as_ref().map(|asking| &asking.name),
            "code": joining.and_then(|joining| joining.code.as_deref()).map(spaced),
            "other": group.other.as_ref().map(|(_, name)| name),
            "blocked": group.blocked,
        })
    })
    .unwrap_or(Value::Null)
}

/// Passes a control request to the member serving Home, if this one is not.
pub async fn forward(request: &Value) -> Option<Result<Value>> {
    let (leader, key) = with(|group| (group.leader, group.key.clone())).ok()?;
    let (leader, key) = (leader?, key?);
    let request = Request::Control {
        request: request.clone(),
    };
    Some(
        ask(leader, &key, &request)
            .await
            .context("the computer serving Home did not answer"),
    )
}

/// Hellos go to every hearth on the network, several on one computer too.
fn udp() -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.set_broadcast(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from(([0, 0, 0, 0], PORT)).into())?;
    Ok(UdpSocket::from_std(socket.into())?)
}

/// This computer's id among the others, kept across restarts.
fn node_id() -> Result<String> {
    let path = config::config_dir()?.join("node");
    if let Some(id) = fs::read_to_string(&path)
        .ok()
        .filter(|id| !id.trim().is_empty())
    {
        return Ok(id.trim().to_owned());
    }
    let id = format!("{:016x}", rand::random::<u64>());
    fs::write(&path, &id)?;
    Ok(id)
}

/// The name people know this computer by.
fn computer_name() -> String {
    let named = |program: &str, args: &[&str]| {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .ok()?;
        let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (output.status.success() && !name.is_empty()).then_some(name)
    };
    named("scutil", &["--get", "ComputerName"])
        .or_else(|| named("hostname", &[]))
        .unwrap_or_else(|| "hearth".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(id: &str, serving: bool) -> Hello {
        Hello {
            id: id.into(),
            name: format!("Mac {id}"),
            serving,
            ..Default::default()
        }
    }

    fn standby(id: &str) -> Role {
        Role::Standby {
            leader: format!("Mac {id}"),
        }
    }

    #[test]
    fn a_serving_member_keeps_serving() {
        let leader = hello("bbb", true);
        assert_eq!(role("aaa", false, &[&leader], 9_000, 0), standby("bbb"));
        assert_eq!(role("ccc", true, &[&leader], 9_000, 0), standby("bbb"));
        assert_eq!(role("aaa", true, &[&leader], 9_000, 0), Role::Serving);
    }

    #[test]
    fn the_lowest_id_serves_once_it_has_listened() {
        let other = hello("ccc", false);
        assert_eq!(role("bbb", false, &[&other], 500, 0), Role::Looking);
        assert_eq!(role("bbb", false, &[&other], 9_000, 0), Role::Serving);
        assert_eq!(role("ddd", false, &[&other], 9_000, 0), standby("ccc"));
        assert_eq!(role("ddd", false, &[], 9_000, 0), Role::Serving);
    }

    #[test]
    fn the_lowest_id_starts_a_group_and_a_group_heard_is_joined() {
        let heard = |id: &str, group: &str| {
            let hello = Hello {
                id: id.into(),
                group: group.into(),
                ..Default::default()
            };
            let peer = Heard {
                hello,
                addr: IpAddr::from([127, 0, 0, 1]),
                seen: 0,
                member: false,
            };
            BTreeMap::from([(id.to_owned(), peer)])
        };
        assert!(!should_found("bbb", 0, 1_000, &BTreeMap::new()));
        assert!(should_found("bbb", 0, 4_000, &BTreeMap::new()));
        assert!(!should_found("bbb", 0, 4_000, &heard("aaa", "")));
        assert!(should_found("bbb", 0, 16_000, &heard("aaa", "")));
        assert!(should_found("bbb", 0, 4_000, &heard("ccc", "")));
        assert!(!should_found("bbb", 0, 60_000, &heard("ccc", "group")));
    }
}
