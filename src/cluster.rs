//! One HomeKit bridge for the computers that have joined the same group.
//!
//! A new computer has to be approved. It shows a code, and someone types that
//! code on a computer that is already in. Until then it hears announcements
//! and nothing else: no device list, no pairing, no keys. After that, every
//! message is sealed. One machine serves Home. If it disappears, another
//! serves the same bridge.
//!
//! A machine that is already serving keeps the job. A second laptop does not
//! take over just by waking up.

mod crypto;
mod wire;

use crate::{bridge, config, config::Config, status};
use anyhow::{Context, Result};
use base64::Engine;
use crypto::{codes_match, derive, mac, mac_ok, open, seal, ClusterKey, Ephemeral};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream, UdpSocket, UnixListener, UnixStream},
    signal::unix::{signal, SignalKind},
    sync::{mpsc, watch},
};
use wire::{
    Approve, Ask, FileBlob, Frame, Handshake, Hello, Reply, Secret, StateBody, DECLINE, DENY,
    GRANT, HELLO, JOIN, JOIN_ACK, LEAVE, REPLY, START, STATE, STATE_ASK,
};

const UDP_PORT: u16 = 47_620;
const TCP_PORT: u16 = 47_621;
const FRESH_MS: u64 = 4_000;
const GRACE_MS: u64 = 1_200;
const FOUND_WAIT_MS: u64 = 3_000;
const LOWER_WAIT_MS: u64 = 15_000;
const HEARD_MS: u64 = 10_000;

struct Gate {
    generation: u64,
    name: String,
    sas: String,
    fails: u8,
    tx: mpsc::Sender<Decision>,
}

enum Decision {
    Approve,
    Deny,
}

struct Membership {
    key: Option<ClusterKey>,
    generation: u64,
    gate: Option<Gate>,
    code: Option<String>,
    host: Option<String>,
    cancel: Option<watch::Sender<bool>>,
    confirm: Option<watch::Sender<bool>>,
}

struct Runtime {
    id: String,
    name: String,
    membership: Mutex<Membership>,
    /// Plain JSON of the state a member may ask for. Sealed per reply.
    body: Mutex<Vec<u8>>,
}

struct Heard {
    name: String,
    cluster: String,
    addr: IpAddr,
    seen_ms: u64,
    peer: Option<Peer>,
}

struct Standing {
    role: &'static str,
    leader: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerView {
    pub id: String,
    pub name: String,
    pub leader: bool,
    pub seen_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    Serve,
    Standby { leader: String },
    Starting,
}

pub fn role(me: &str, i_lead: bool, peers: &[PeerView], now_ms: u64, started_ms: u64) -> Role {
    let fresh = |seen: u64| now_ms.saturating_sub(seen) < FRESH_MS;
    let mut alive: Vec<(&str, &str, bool)> = peers
        .iter()
        .filter(|peer| fresh(peer.seen_ms))
        .map(|peer| (peer.id.as_str(), peer.name.as_str(), peer.leader))
        .collect();
    alive.push((me, "", i_lead));

    if let Some((id, name, _)) = alive
        .iter()
        .filter(|(_, _, leader)| *leader)
        .min_by_key(|(id, _, _)| *id)
    {
        return if *id == me {
            Role::Serve
        } else {
            Role::Standby {
                leader: (*name).to_owned(),
            }
        };
    }

    // Nobody is serving yet. Wait long enough to hear the other machines,
    // so two laptops opening together do not both advertise the bridge.
    if !i_lead && now_ms.saturating_sub(started_ms) < GRACE_MS {
        return Role::Starting;
    }

    let (id, name, _) = alive
        .iter()
        .min_by_key(|(id, _, _)| *id)
        .expect("this machine is in the list");
    if *id == me {
        Role::Serve
    } else {
        Role::Standby {
            leader: (*name).to_owned(),
        }
    }
}

pub async fn run() -> Result<()> {
    let runtime = Arc::new(Runtime {
        id: node_id()?,
        name: machine_name(),
        membership: Mutex::new(Membership {
            key: ClusterKey::load().ok(),
            generation: 0,
            gate: None,
            code: None,
            host: None,
            cancel: None,
            confirm: None,
        }),
        body: Mutex::new(Vec::new()),
    });
    let started = now_ms();
    let udp = UdpSocket::bind(("0.0.0.0", UDP_PORT))
        .await
        .context("hearth is already running on this machine")?;
    udp.set_broadcast(true)?;
    let tcp = TcpListener::bind(("0.0.0.0", TCP_PORT))
        .await
        .context("hearth is already running on this machine")?;
    let control = bind_control()?;
    let accepted = Arc::clone(&runtime);
    tokio::spawn(async move { accept_peers(tcp, accepted).await });
    let controlled = Arc::clone(&runtime);
    tokio::spawn(async move { accept_control(control, controlled).await });

    let (join_tx, mut join_rx) = mpsc::channel::<(u64, Result<ClusterKey>)>(1);
    let mut heard: BTreeMap<String, Heard> = BTreeMap::new();
    let mut serving: Option<Server> = None;
    let mut i_lead = false;
    let mut conflict = false;
    let mut joining = false;
    let mut next_join_ms = 0u64;
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut buffer = [0u8; 2048];
    let mut terminate = signal(SignalKind::terminate()).context("could not listen for stop")?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                shutdown(&mut serving).await;
                break;
            }
            _ = terminate.recv() => {
                shutdown(&mut serving).await;
                break;
            }
            Some((generation, result)) = join_rx.recv() => {
                joining = false;
                finish_join(&runtime, generation, result);
            }
            _ = tick.tick() => {
                let now = now_ms();
                heard.retain(|_, item| now.saturating_sub(item.seen_ms) < HEARD_MS);
                let config = load_config()?;
                refresh_body(&runtime, &config)?;
                maybe_found(&runtime, &heard, started, now)?;
                announce(&udp, &runtime, &config, i_lead).await;
                let standing = if member_key(&runtime).is_some() {
                    reconcile(
                        &runtime,
                        started,
                        &heard,
                        &mut serving,
                        &mut i_lead,
                        &mut conflict,
                    )
                    .await?
                } else {
                    stop_server(&mut serving).await;
                    i_lead = false;
                    consider_join(&runtime, &heard, &join_tx, &mut joining, &mut next_join_ms, now);
                    let host = runtime.membership.lock().expect("membership").host.clone();
                    Standing {
                        role: if host.is_some() { "joining" } else { "starting" },
                        leader: host.unwrap_or_default(),
                    }
                };
                publish_note(&runtime, &config, &standing, &heard);
            }
            result = udp.recv_from(&mut buffer) => {
                let (size, from) = result.context("could not hear other machines")?;
                if let Some((id, item)) = hear(&buffer[..size], from.ip(), member_key(&runtime).as_ref()) {
                    if id != runtime.id {
                        heard.insert(id, item);
                    }
                }
            }
        }
    }
    Ok(())
}

async fn shutdown(serving: &mut Option<Server>) {
    stop_server(serving).await;
    status::finish();
}

#[derive(Clone)]
struct Peer {
    view: PeerView,
    addr: IpAddr,
    bridge: String,
    paired: bool,
    config_rev: u64,
    storage: String,
}

struct Server {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    signature: String,
}

async fn reconcile(
    runtime: &Runtime,
    started: u64,
    heard: &BTreeMap<String, Heard>,
    serving: &mut Option<Server>,
    i_lead: &mut bool,
    conflict: &mut bool,
) -> Result<Standing> {
    let now = now_ms();
    let peers: Vec<Peer> = heard
        .values()
        .filter_map(|item| item.peer.clone())
        .collect();
    let mut config = load_config()?;
    let local_storage = storage_hash();
    let key = member_key(runtime).context("not in a group")?;
    for peer in &peers {
        if !wants_state(
            config::revision(&config),
            &config.bridge_id,
            config::paired(),
            &local_storage,
            peer,
        ) {
            continue;
        }
        let Ok(remote) = fetch(peer.addr, &key, &runtime.id).await else {
            continue;
        };
        let merged = config::merge(config.clone(), remote.config, config::paired(), peer.paired);
        if merged.conflict {
            continue;
        }
        let changed = merged.config != config;
        let adopt = merged.adopt_identity;
        config = merged.config;
        if changed {
            config::save(&config)?;
        }
        // Copy the pairing only while this process is not serving. Replacing
        // the files under a live bridge would cut Home off mid-session.
        if !remote.files.is_empty() && (adopt || (peer.view.leader && !*i_lead)) {
            stop_server(serving).await;
            install_storage(&remote.files)?;
        }
    }
    refresh_body(runtime, &config)?;
    *conflict = peers.iter().any(|peer| {
        peer.paired
            && config::paired()
            && !peer.bridge.is_empty()
            && !config.bridge_id.is_empty()
            && peer.bridge != config.bridge_id
    });

    let views: Vec<PeerView> = peers
        .iter()
        .filter(|peer| {
            peer.bridge.is_empty() || config.bridge_id.is_empty() || peer.bridge == config.bridge_id
        })
        .map(|peer| peer.view.clone())
        .collect();
    let decided = role(&runtime.id, *i_lead, &views, now, started);
    let signature = signature(&config);
    let standing = match decided {
        Role::Serve => {
            let restart = serving
                .as_ref()
                .is_none_or(|server| server.signature != signature);
            if restart {
                stop_server(serving).await;
                *serving = Some(spawn_server(config, signature).await?);
            }
            *i_lead = true;
            Standing {
                role: if *conflict { "conflict" } else { "leader" },
                leader: runtime.name.clone(),
            }
        }
        Role::Standby { leader } => {
            stop_server(serving).await;
            *i_lead = false;
            Standing {
                role: if *conflict { "conflict" } else { "standby" },
                leader,
            }
        }
        Role::Starting => {
            stop_server(serving).await;
            *i_lead = false;
            Standing {
                role: "starting",
                leader: String::new(),
            }
        }
    };
    Ok(standing)
}

/// Whether this machine should pull another machine's config and pairing.
fn wants_state(
    local_rev: u64,
    local_bridge: &str,
    local_paired: bool,
    local_storage: &str,
    peer: &Peer,
) -> bool {
    let different =
        !peer.bridge.is_empty() && !local_bridge.is_empty() && peer.bridge != local_bridge;
    // Two Homes that are already paired stay as they are.
    if different && peer.paired && local_paired {
        return false;
    }
    let newer = peer.config_rev > local_rev;
    let need_identity = different || (peer.paired && !local_paired);
    let need_storage =
        peer.view.leader && !different && peer.storage != local_storage && peer.storage != "0";
    newer || need_identity || need_storage
}

async fn spawn_server(config: Config, signature: String) -> Result<Server> {
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        if let Err(error) = bridge::serve(config, rx).await {
            log::error!("HomeKit stopped: {error:#}");
        }
    });
    Ok(Server {
        stop,
        task,
        signature,
    })
}

async fn stop_server(serving: &mut Option<Server>) {
    let Some(server) = serving.take() else {
        return;
    };
    let _ = server.stop.send(true);
    let mut task = server.task;
    if tokio::time::timeout(Duration::from_secs(3), &mut task)
        .await
        .is_err()
    {
        task.abort();
    }
}

fn signature(config: &Config) -> String {
    let mut parts = vec![
        config.bridge_id.clone(),
        config.homekit_pin.clone(),
        config.name.clone(),
    ];
    for device in &config.devices {
        parts.push(format!(
            "{}:{}:{}:{}",
            device.key(),
            device.updated(),
            device.removed(),
            device.name()
        ));
    }
    for (key, value) in &config.names {
        parts.push(format!("name:{key}={value}"));
    }
    parts.join("\n")
}

async fn announce(udp: &UdpSocket, runtime: &Runtime, config: &Config, leader: bool) {
    let Some(key) = member_key(runtime) else {
        let _ = send_hello(udp, &runtime.id, &runtime.name, config, "", "", leader).await;
        return;
    };
    let mut hello = unsigned_hello(&runtime.id, &runtime.name, config, &key.id, leader);
    hello.mac = crypto::encode_hex(&mac(&key.hello_key(), &hello.material()));
    let Ok(body) = wire::to_json(&hello) else {
        return;
    };
    let Ok(frame) = wire::encode(HELLO, &body) else {
        return;
    };
    let _ = udp.send_to(&frame, ("255.255.255.255", UDP_PORT)).await;
}

async fn send_hello(
    udp: &UdpSocket,
    id: &str,
    name: &str,
    config: &Config,
    cluster: &str,
    mac_hex: &str,
    leader: bool,
) -> Result<()> {
    let mut hello = unsigned_hello(id, name, config, cluster, leader);
    hello.mac = mac_hex.to_owned();
    let body = wire::to_json(&hello)?;
    let frame = wire::encode(HELLO, &body)?;
    udp.send_to(&frame, ("255.255.255.255", UDP_PORT)).await?;
    Ok(())
}

fn unsigned_hello(id: &str, name: &str, config: &Config, cluster: &str, leader: bool) -> Hello {
    Hello {
        id: id.to_owned(),
        name: name.to_owned(),
        cluster: cluster.to_owned(),
        bridge: config.bridge_id.clone(),
        paired: config::paired(),
        config_rev: config::revision(config),
        storage: storage_hash(),
        leader,
        mac: String::new(),
    }
}

fn hear(bytes: &[u8], addr: IpAddr, key: Option<&ClusterKey>) -> Option<(String, Heard)> {
    let frame = wire::decode(bytes).ok()?;
    if frame.kind != HELLO {
        return None;
    }
    let hello: Hello = wire::from_json(&frame.body).ok()?;
    if hello.id.is_empty() {
        return None;
    }
    let peer = key.and_then(|key| verified_peer(&hello, key, addr));
    if hello.cluster == key.map(|key| key.id.as_str()).unwrap_or("")
        && peer.is_none()
        && !hello.cluster.is_empty()
    {
        // Same group, but the check failed. Ignore it.
        return None;
    }
    Some((
        hello.id.clone(),
        Heard {
            name: hello.name,
            cluster: hello.cluster,
            addr,
            seen_ms: now_ms(),
            peer,
        },
    ))
}

fn verified_peer(hello: &Hello, key: &ClusterKey, addr: IpAddr) -> Option<Peer> {
    if hello.cluster != key.id {
        return None;
    }
    let tag = crypto::decode_hex(&hello.mac).ok()?;
    if !mac_ok(&key.hello_key(), &hello.material(), &tag) {
        return None;
    }
    Some(Peer {
        view: PeerView {
            id: hello.id.clone(),
            name: hello.name.clone(),
            leader: hello.leader,
            seen_ms: now_ms(),
        },
        addr,
        bridge: hello.bridge.clone(),
        paired: hello.paired,
        config_rev: hello.config_rev,
        storage: hello.storage.clone(),
    })
}

struct Remote {
    config: Config,
    files: BTreeMap<String, Vec<u8>>,
}

async fn fetch(addr: IpAddr, key: &ClusterKey, id: &str) -> Result<Remote> {
    let mut stream = connect_tcp(addr).await?;
    let ask = Ask { id: id.to_owned() };
    let sealed = seal(&key.state_key(), b"ask", &wire::to_json(&ask)?)?;
    write_kind(&mut stream, STATE_ASK, &sealed).await?;
    let frame = read_kind(&mut stream).await?;
    if frame.kind != STATE {
        anyhow::bail!("the other computer did not share its state");
    }
    decode_state(&key.state_key(), &frame.body)
}

fn decode_state(key: &[u8; 32], body: &[u8]) -> Result<Remote> {
    let plain = open(key, b"state", body)?;
    let state: StateBody = wire::from_json(&plain)?;
    let config = config::config_from_toml(&state.config)?;
    let mut files = BTreeMap::new();
    for file in state.files {
        if !safe_name(&file.name) {
            continue;
        }
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(file.bytes) {
            files.insert(file.name, bytes);
        }
    }
    Ok(Remote { config, files })
}

fn safe_name(name: &str) -> bool {
    !name.is_empty() && !name.contains("..") && !name.starts_with('/') && !name.contains('\\')
}

async fn accept_peers(listener: TcpListener, runtime: Arc<Runtime>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move {
            if let Err(error) = handle_peer(stream, &runtime).await {
                log::debug!("peer closed: {error:#}");
            }
        });
    }
}

async fn handle_peer(mut stream: TcpStream, runtime: &Runtime) -> Result<()> {
    let frame = tokio::time::timeout(Duration::from_secs(5), read_kind(&mut stream))
        .await
        .context("no request")??;
    match frame.kind {
        JOIN => handle_join(stream, &frame.body, runtime).await,
        STATE_ASK => handle_state(stream, &frame.body, runtime).await,
        _ => Ok(()),
    }
}

async fn handle_state(mut stream: TcpStream, body: &[u8], runtime: &Runtime) -> Result<()> {
    let key = member_key(runtime).context("not in a group")?;
    open(&key.state_key(), b"ask", body)?;
    let plain = runtime
        .body
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    let sealed = seal(&key.state_key(), b"state", &plain)?;
    write_kind(&mut stream, STATE, &sealed).await
}

async fn handle_join(mut stream: TcpStream, body: &[u8], runtime: &Runtime) -> Result<()> {
    let open_msg: Handshake = wire::from_json(body)?;
    let key = member_key(runtime).context("not in a group")?;
    if runtime
        .membership
        .lock()
        .expect("membership")
        .gate
        .is_some()
    {
        let deny = wire::to_json(&Reply {
            ok: false,
            error: "another computer is already asking".into(),
        })?;
        write_kind(&mut stream, DENY, &deny).await?;
        return Ok(());
    }
    let eph = Ephemeral::generate();
    let peer_pub = crypto::parse_public(&open_msg.eph)?;
    let member_pub = eph.public;
    let shared = eph.agree(&peer_pub);
    let code = crypto::sas(&shared, &member_pub, &peer_pub);
    let ack = Handshake {
        id: runtime.id.clone(),
        name: runtime.name.clone(),
        eph: crypto::encode_hex(&member_pub),
    };
    write_kind(&mut stream, JOIN_ACK, &wire::to_json(&ack)?).await?;
    let (tx, mut rx) = mpsc::channel(1);
    let generation = {
        let mut membership = runtime.membership.lock().expect("membership");
        membership.generation = membership.generation.wrapping_add(1);
        let generation = membership.generation;
        membership.gate = Some(Gate {
            generation,
            name: open_msg.name.clone(),
            sas: code.clone(),
            fails: 0,
            tx,
        });
        generation
    };
    log::info!("{} wants to join", open_msg.name);
    let decision = tokio::time::timeout(Duration::from_secs(180), rx.recv()).await;
    {
        let mut membership = runtime.membership.lock().expect("membership");
        if membership
            .gate
            .as_ref()
            .is_some_and(|gate| gate.generation == generation)
        {
            membership.gate = None;
        }
    }
    let approved = matches!(decision, Ok(Some(Decision::Approve)));
    if !approved {
        let deny = wire::to_json(&Reply {
            ok: false,
            error: "not approved".into(),
        })?;
        let _ = write_kind(&mut stream, DENY, &deny).await;
        return Ok(());
    }
    let secret = Secret {
        id: key.id.clone(),
        key: key.encode_key(),
    };
    let wrap = derive(&shared, b"hearth-join-v1");
    let aad = join_aad(&peer_pub, &member_pub, &open_msg.id, &runtime.id);
    let sealed = seal(&wrap, &aad, &wire::to_json(&secret)?)?;
    write_kind(&mut stream, GRANT, &sealed).await
}

fn join_aad(joiner: &[u8], member: &[u8], joiner_id: &str, member_id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + joiner_id.len() + member_id.len());
    out.extend(joiner);
    out.extend(member);
    out.extend(joiner_id.as_bytes());
    out.extend(member_id.as_bytes());
    out
}

async fn accept_control(listener: UnixListener, runtime: Arc<Runtime>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move {
            let _ = handle_control(stream, &runtime).await;
        });
    }
}

async fn handle_control(mut stream: UnixStream, runtime: &Runtime) -> Result<()> {
    let frame = tokio::time::timeout(Duration::from_secs(2), read_kind(&mut stream)).await??;
    let reply = match frame.kind {
        wire::APPROVE => match wire::from_json::<Approve>(&frame.body) {
            Ok(request) => accept_code(runtime, &request.code),
            Err(error) => Reply {
                ok: false,
                error: error.to_string(),
            },
        },
        DECLINE => {
            deny(runtime);
            Reply {
                ok: true,
                error: String::new(),
            }
        }
        LEAVE => {
            leave_group(runtime);
            Reply {
                ok: true,
                error: String::new(),
            }
        }
        wire::CONFIRM => confirm_join(runtime),
        START => match start_group(runtime) {
            Ok(()) => Reply {
                ok: true,
                error: String::new(),
            },
            Err(error) => Reply {
                ok: false,
                error: error.to_string(),
            },
        },
        _ => Reply {
            ok: false,
            error: "unknown request".into(),
        },
    };
    write_kind(&mut stream, REPLY, &wire::to_json(&reply)?).await
}

fn accept_code(runtime: &Runtime, typed: &str) -> Reply {
    let mut membership = runtime.membership.lock().expect("membership");
    let Some(gate) = membership.gate.as_mut() else {
        return Reply {
            ok: false,
            error: "no computer is asking to join".into(),
        };
    };
    if !codes_match(&gate.sas, typed) {
        gate.fails = gate.fails.saturating_add(1);
        if gate.fails >= 5 {
            let _ = gate.tx.try_send(Decision::Deny);
        }
        return Reply {
            ok: false,
            error: "that code does not match".into(),
        };
    }
    let _ = gate.tx.try_send(Decision::Approve);
    Reply {
        ok: true,
        error: String::new(),
    }
}

fn confirm_join(runtime: &Runtime) -> Reply {
    let membership = runtime.membership.lock().expect("membership");
    let Some(confirm) = membership.confirm.as_ref() else {
        return Reply {
            ok: false,
            error: "this computer is not waiting to join".into(),
        };
    };
    let _ = confirm.send(true);
    Reply {
        ok: true,
        error: String::new(),
    }
}

fn deny(runtime: &Runtime) {
    if let Some(gate) = runtime.membership.lock().expect("membership").gate.as_ref() {
        let _ = gate.tx.try_send(Decision::Deny);
    }
}

fn leave_group(runtime: &Runtime) {
    cancel_join(runtime);
    let mut membership = runtime.membership.lock().expect("membership");
    membership.key = None;
    membership.generation = membership.generation.wrapping_add(1);
    drop(membership);
    let _ = ClusterKey::delete();
}

fn start_group(runtime: &Runtime) -> Result<()> {
    if member_key(runtime).is_some() {
        return Ok(());
    }
    cancel_join(runtime);
    let key = ClusterKey::generate();
    key.save()?;
    let mut membership = runtime.membership.lock().expect("membership");
    membership.generation = membership.generation.wrapping_add(1);
    membership.key = Some(key);
    membership.code = None;
    membership.host = None;
    Ok(())
}

fn cancel_join(runtime: &Runtime) {
    if let Some(cancel) = runtime.membership.lock().expect("membership").cancel.take() {
        let _ = cancel.send(true);
    }
}

fn maybe_found(
    runtime: &Runtime,
    heard: &BTreeMap<String, Heard>,
    started: u64,
    now: u64,
) -> Result<()> {
    if member_key(runtime).is_some() {
        return Ok(());
    }
    let sights: Vec<Sight> = heard
        .iter()
        .map(|(id, item)| Sight {
            id: id.clone(),
            cluster: item.cluster.clone(),
            seen_ms: item.seen_ms,
        })
        .collect();
    if !should_found(&runtime.id, started, now, &sights) {
        return Ok(());
    }
    start_group(runtime)
}

fn consider_join(
    runtime: &Arc<Runtime>,
    heard: &BTreeMap<String, Heard>,
    join_tx: &mpsc::Sender<(u64, Result<ClusterKey>)>,
    joining: &mut bool,
    next_join_ms: &mut u64,
    now: u64,
) {
    if *joining || now < *next_join_ms {
        return;
    }
    let Some((_, target)) = heard
        .iter()
        .filter(|(_, item)| !item.cluster.is_empty())
        .min_by_key(|(id, _)| *id)
    else {
        let mut membership = runtime.membership.lock().expect("membership");
        membership.code = None;
        membership.host = None;
        return;
    };
    *joining = true;
    *next_join_ms = now.saturating_add(5_000);
    let addr = target.addr;
    let host = target.name.clone();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let generation = {
        let mut membership = runtime.membership.lock().expect("membership");
        membership.cancel = Some(cancel_tx);
        membership.host = Some(host);
        membership.generation
    };
    let id = runtime.id.clone();
    let name = runtime.name.clone();
    let noted = Arc::clone(runtime);
    let tx = join_tx.clone();
    tokio::spawn(async move {
        let result = join_peer(addr, &id, &name, cancel_rx, &noted).await;
        let _ = tx.send((generation, result)).await;
    });
}

fn storage_files() -> (String, Vec<FileBlob>) {
    let mut raw = BTreeMap::new();
    let Ok(dir) = config::homekit_dir() else {
        return ("0".into(), Vec::new());
    };
    if walk(&dir, &dir, &mut raw).is_err() || raw.is_empty() {
        return ("0".into(), Vec::new());
    }
    let mut hasher = Sha256::new();
    let mut total = 0usize;
    for (name, bytes) in &raw {
        hasher.update(name.as_bytes());
        hasher.update(bytes);
        total = total.saturating_add(bytes.len());
    }
    let hash = hex(&hasher.finalize());
    // A partial copy would replace a good pairing with a broken one.
    if total > 2_000_000 {
        return (hash, Vec::new());
    }
    let files = raw
        .into_iter()
        .filter(|(name, _)| safe_name(name))
        .map(|(name, bytes)| FileBlob {
            name,
            bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
        .collect();
    (hash, files)
}

fn storage_hash() -> String {
    storage_files().0
}

fn walk(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out)?;
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let name = rel.to_string_lossy().replace('\\', "/");
        if let Ok(bytes) = std::fs::read(&path) {
            out.insert(name, bytes);
        }
    }
    Ok(())
}

fn install_storage(files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let dir = config::homekit_dir()?;
    let tmp = config::config_dir()?.join("homekit.next");
    let old = config::config_dir()?.join("homekit.old");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    for (name, bytes) in files {
        if name.contains("..") || name.starts_with('/') {
            continue;
        }
        let path = tmp.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
    }
    let _ = std::fs::remove_dir_all(&old);
    if dir.exists() {
        std::fs::rename(&dir, &old)?;
    }
    std::fs::rename(&tmp, &dir)?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

fn load_config() -> Result<Config> {
    let path = config::config_path()?;
    let mut config = if path.exists() {
        config::load()?
    } else {
        config::load_or_new()?
    };
    if config::ensure_bridge_id(&mut config) || !path.exists() {
        config::save(&config)?;
    }
    Ok(config)
}

fn node_id() -> Result<String> {
    let path = config::config_dir()?.join("node");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let text = text.trim();
        if !text.is_empty() {
            return Ok(text.to_owned());
        }
    }
    std::fs::create_dir_all(config::config_dir()?)?;
    let id = format!(
        "{:016x}{:016x}",
        rand::random::<u64>(),
        rand::random::<u64>()
    );
    std::fs::write(&path, &id)?;
    Ok(id)
}

pub fn machine_name() -> String {
    if let Ok(output) = std::process::Command::new("hostname").output() {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !name.is_empty() {
            return name;
        }
    }
    "hearth".into()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    crypto::encode_hex(bytes)
}

fn member_key(runtime: &Runtime) -> Option<ClusterKey> {
    runtime
        .membership
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .key
        .clone()
}

fn refresh_body(runtime: &Runtime, config: &Config) -> Result<()> {
    let (storage, files) = storage_files();
    let body = StateBody {
        config: toml::to_string(config)?,
        storage,
        files,
    };
    *runtime
        .body
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = wire::to_json(&body)?;
    Ok(())
}

fn publish_note(
    runtime: &Runtime,
    config: &Config,
    standing: &Standing,
    heard: &BTreeMap<String, Heard>,
) {
    let membership = runtime.membership.lock().expect("membership");
    let ask = membership.gate.as_ref().map(|gate| gate.name.clone());
    let code = membership
        .code
        .clone()
        .map(|digits| crypto::format_code(&digits));
    let ours = membership.key.as_ref().map(|key| key.id.clone());
    drop(membership);
    let other = ours.and_then(|id| {
        heard
            .values()
            .find(|item| !item.cluster.is_empty() && item.cluster != id)
            .map(|item| item.name.clone())
    });
    status::set_cluster_note(config, standing.role, &standing.leader, ask, code, other);
}

struct Sight {
    id: String,
    cluster: String,
    seen_ms: u64,
}

fn should_found(my_id: &str, started_ms: u64, now_ms: u64, sights: &[Sight]) -> bool {
    let fresh = |seen: u64| now_ms.saturating_sub(seen) < HEARD_MS;
    if now_ms.saturating_sub(started_ms) < FOUND_WAIT_MS {
        return false;
    }
    if sights
        .iter()
        .any(|sight| fresh(sight.seen_ms) && !sight.cluster.is_empty())
    {
        return false;
    }
    let lower = sights
        .iter()
        .any(|sight| fresh(sight.seen_ms) && sight.cluster.is_empty() && sight.id.as_str() < my_id);
    if lower && now_ms.saturating_sub(started_ms) < LOWER_WAIT_MS {
        return false;
    }
    true
}

fn finish_join(runtime: &Runtime, generation: u64, result: Result<ClusterKey>) {
    let mut membership = runtime.membership.lock().expect("membership");
    membership.code = None;
    membership.host = None;
    membership.cancel = None;
    membership.confirm = None;
    if membership.generation != generation || membership.key.is_some() {
        return;
    }
    match result {
        Ok(key) => {
            if key.save().is_ok() {
                membership.key = Some(key);
            }
        }
        Err(error) => log::info!("join did not finish: {error:#}"),
    }
}

async fn join_peer(
    addr: IpAddr,
    id: &str,
    name: &str,
    mut cancel: watch::Receiver<bool>,
    runtime: &Runtime,
) -> Result<ClusterKey> {
    let eph = Ephemeral::generate();
    let eph_pub = eph.public;
    let mut stream = connect_tcp(addr).await?;
    let open_msg = Handshake {
        id: id.to_owned(),
        name: name.to_owned(),
        eph: crypto::encode_hex(&eph_pub),
    };
    write_kind(&mut stream, JOIN, &wire::to_json(&open_msg)?).await?;
    let frame = tokio::select! {
        _ = cancel.changed() => anyhow::bail!("stopped waiting to join"),
        frame = tokio::time::timeout(Duration::from_secs(5), read_kind(&mut stream)) => {
            frame.context("the other computer did not answer")??
        }
    };
    if frame.kind == DENY {
        anyhow::bail!("the other computer declined");
    }
    if frame.kind != JOIN_ACK {
        anyhow::bail!("the other computer did not accept the join");
    }
    let ack: Handshake = wire::from_json(&frame.body)?;
    let member_pub = crypto::parse_public(&ack.eph)?;
    let shared = eph.agree(&member_pub);
    let code = crypto::sas(&shared, &eph_pub, &member_pub);
    let (confirm_tx, mut confirm_rx) = watch::channel(false);
    {
        let mut membership = runtime.membership.lock().expect("membership");
        membership.code = Some(code);
        membership.confirm = Some(confirm_tx);
    }
    let frame = tokio::select! {
        _ = cancel.changed() => anyhow::bail!("stopped waiting to join"),
        frame = tokio::time::timeout(Duration::from_secs(180), read_kind(&mut stream)) => {
            frame.context("the other computer did not approve")??
        }
    };
    if frame.kind != GRANT {
        anyhow::bail!("the other computer declined");
    }
    if !*confirm_rx.borrow() {
        tokio::select! {
            _ = cancel.changed() => anyhow::bail!("stopped waiting to join"),
            result = confirm_rx.changed() => {
                result.context("stopped waiting to join")?;
            }
        }
    }
    if !*confirm_rx.borrow() {
        anyhow::bail!("stopped waiting to join");
    }
    let wrap = derive(&shared, b"hearth-join-v1");
    let aad = join_aad(&eph_pub, &member_pub, id, &ack.id);
    let plain = open(&wrap, &aad, &frame.body)?;
    let secret: Secret = wire::from_json(&plain)?;
    ClusterKey::from_parts(&secret.id, &secret.key)
}

fn bind_control() -> Result<UnixListener> {
    let dir = config::config_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("hearth.sock");
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
}

async fn connect_tcp(addr: IpAddr) -> Result<TcpStream> {
    tokio::time::timeout(Duration::from_secs(3), TcpStream::connect((addr, TCP_PORT)))
        .await
        .context("the other computer did not answer")?
        .context("could not reach the other computer")
}

async fn write_kind<W>(io: &mut W, kind: u8, body: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    wire::write_frame(io, kind, body).await
}

async fn read_kind<R>(io: &mut R) -> Result<Frame>
where
    R: AsyncRead + Unpin,
{
    wire::read_frame(io).await
}

async fn control_request(kind: u8, body: &[u8]) -> Result<Reply> {
    let path = config::config_dir()?.join("hearth.sock");
    let mut stream = UnixStream::connect(&path)
        .await
        .context("the bridge is not running")?;
    write_kind(&mut stream, kind, body).await?;
    let frame = tokio::time::timeout(Duration::from_secs(3), read_kind(&mut stream))
        .await
        .context("the bridge did not answer")??;
    if frame.kind != REPLY {
        anyhow::bail!("the bridge sent an unexpected reply");
    }
    wire::from_json(&frame.body)
}

pub async fn approve(code: &str) -> Result<()> {
    let reply = control_request(
        wire::APPROVE,
        &wire::to_json(&Approve {
            code: code.to_owned(),
        })?,
    )
    .await?;
    if !reply.ok {
        anyhow::bail!("{}", reply.error);
    }
    println!("Approved.");
    Ok(())
}

pub async fn confirm() -> Result<()> {
    let reply = control_request(wire::CONFIRM, b"{}").await?;
    if !reply.ok {
        anyhow::bail!("{}", reply.error);
    }
    println!("Confirmed.");
    Ok(())
}

pub async fn decline() -> Result<()> {
    let reply = control_request(DECLINE, b"{}").await?;
    if !reply.ok {
        anyhow::bail!("{}", reply.error);
    }
    println!("Declined.");
    Ok(())
}

pub async fn leave() -> Result<()> {
    if let Err(error) = control_request(LEAVE, b"{}").await {
        log::debug!("leave, bridge not running: {error:#}");
    }
    ClusterKey::delete()?;
    println!("Left the group. The bridge will ask to join if another computer is on the network.");
    Ok(())
}

pub async fn start_own() -> Result<()> {
    match control_request(START, b"{}").await {
        Ok(reply) if !reply.ok => anyhow::bail!("{}", reply.error),
        Ok(_) => {}
        Err(_) => {
            if ClusterKey::load().is_err() {
                let key = ClusterKey::generate();
                key.save()?;
            }
        }
    }
    println!("This computer has its own group.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: &str, name: &str, leader: bool, seen: u64) -> PeerView {
        PeerView {
            id: id.into(),
            name: name.into(),
            leader,
            seen_ms: seen,
        }
    }

    #[test]
    fn a_live_leader_is_not_replaced_by_a_lower_id() {
        let peers = vec![peer("bbb", "other", true, 10_000)];
        let decided = role("aaa", false, &peers, 11_000, 0);
        assert_eq!(
            decided,
            Role::Standby {
                leader: "other".into()
            }
        );
    }

    #[test]
    fn the_lowest_id_serves_when_nobody_does() {
        let peers = vec![peer("ccc", "other", false, 5_000)];
        assert_eq!(role("bbb", false, &peers, 8_000, 0), Role::Serve);
        assert_eq!(
            role("ddd", false, &peers, 8_000, 0),
            Role::Standby {
                leader: "other".into()
            }
        );
    }

    #[test]
    fn a_quiet_leader_is_replaced() {
        let peers = vec![peer("aaa", "gone", true, 1_000)];
        assert_eq!(role("bbb", false, &peers, 9_000, 0), Role::Serve);
    }

    #[test]
    fn two_machines_wait_before_either_serves() {
        let peers = vec![peer("bbb", "other", false, 1_000)];
        assert_eq!(role("ccc", false, &peers, 1_500, 1_000), Role::Starting);
        assert_eq!(role("aaa", false, &[], 500, 0), Role::Starting);
        assert_eq!(role("aaa", true, &[], 500, 0), Role::Serve);
    }

    #[test]
    fn the_lowest_id_founds_and_a_seen_group_is_joined() {
        let looking = Sight {
            id: "bbb".into(),
            cluster: String::new(),
            seen_ms: 1_000,
        };
        let grouped = Sight {
            id: "ccc".into(),
            cluster: "abc".into(),
            seen_ms: 15_000,
        };
        assert!(!should_found("aaa", 0, 1_000, &[]));
        assert!(should_found("aaa", 0, 4_000, &[]));
        assert!(!should_found("ccc", 0, 4_000, &[looking]));
        assert!(should_found(
            "ccc",
            0,
            16_000,
            &[Sight {
                id: "bbb".into(),
                cluster: String::new(),
                seen_ms: 1_000,
            }]
        ));
        assert!(!should_found("aaa", 0, 20_000, &[grouped]));
    }

    fn sample_peer(bridge: &str, paired: bool, leader: bool, rev: u64, storage: &str) -> Peer {
        Peer {
            view: peer("peer", "other", leader, 0),
            addr: "127.0.0.1".parse().unwrap(),
            bridge: bridge.into(),
            paired,
            config_rev: rev,
            storage: storage.into(),
        }
    }

    #[test]
    fn a_different_unpaired_bridge_is_fetched_and_two_homes_are_not() {
        let other = sample_peer("hearth-bbbb", false, false, 1, "0");
        assert!(wants_state(1, "hearth-aaaa", false, "0", &other));
        let homes = sample_peer("hearth-bbbb", true, true, 1, "abc");
        assert!(!wants_state(9, "hearth-aaaa", true, "abc", &homes));
    }

    #[test]
    fn a_follower_copies_the_leader_pairing_when_it_changes() {
        let leader = sample_peer("same", true, true, 3, "new");
        assert!(wants_state(3, "same", true, "old", &leader));
        assert!(!wants_state(3, "same", true, "new", &leader));
    }
}
