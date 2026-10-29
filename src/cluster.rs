//! One HomeKit bridge for every hearth on the local network.
//!
//! HomeKit can pair a bridge once. Two processes advertising it at the same
//! time look like a broken accessory. The machines gossip, share the device
//! list, and share the pairing. One of them serves Home. If it disappears,
//! another starts serving as the same bridge, so Home does not have to pair again.
//!
//! A machine that is already serving keeps the job. A second laptop does not
//! take over just by waking up.

use crate::{bridge, config, config::Config, status};
use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    signal::unix::{signal, SignalKind},
    sync::watch,
};

const UDP_PORT: u16 = 47_620;
const TCP_PORT: u16 = 47_621;
const FRESH_MS: u64 = 4_000;
const GRACE_MS: u64 = 1_200;

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

#[derive(Debug, Serialize, Deserialize)]
struct Hello {
    v: u8,
    id: String,
    name: String,
    bridge: String,
    paired: bool,
    config_rev: u64,
    storage: String,
    leader: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    config: String,
    storage: String,
    files: BTreeMap<String, String>,
}

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

pub async fn run() -> Result<()> {
    let id = node_id()?;
    let name = machine_name();
    let started = now_ms();
    let shared = Arc::new(Mutex::new(Vec::new()));
    let udp = UdpSocket::bind(("0.0.0.0", UDP_PORT))
        .await
        .context("hearth is already running on this machine")?;
    udp.set_broadcast(true)?;
    let tcp = TcpListener::bind(("0.0.0.0", TCP_PORT))
        .await
        .context("hearth is already running on this machine")?;
    tokio::spawn(accept(tcp, Arc::clone(&shared)));

    let mut peers: BTreeMap<String, Peer> = BTreeMap::new();
    let mut serving: Option<Server> = None;
    let mut i_lead = false;
    let mut conflict = false;
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
            _ = tick.tick() => {
                let config = load_config()?;
                publish_shared(&shared, &config)?;
                announce(&udp, &id, &name, &config, i_lead).await;
                reconcile(&id, &name, started, &mut peers, &shared, &mut serving, &mut i_lead, &mut conflict).await?;
            }
            result = udp.recv_from(&mut buffer) => {
                let (size, from) = result.context("could not hear other machines")?;
                if let Some(peer) = decode_hello(&buffer[..size], from.ip()) {
                    if peer.view.id != id {
                        peers.insert(peer.view.id.clone(), peer);
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

#[allow(clippy::too_many_arguments)]
async fn reconcile(
    id: &str,
    name: &str,
    started: u64,
    peers: &mut BTreeMap<String, Peer>,
    shared: &Arc<Mutex<Vec<u8>>>,
    serving: &mut Option<Server>,
    i_lead: &mut bool,
    conflict: &mut bool,
) -> Result<()> {
    let now = now_ms();
    peers.retain(|_, peer| now.saturating_sub(peer.view.seen_ms) < FRESH_MS * 3);

    let mut config = load_config()?;
    let local_storage = storage_hash();
    for peer in peers.values() {
        if !wants_state(
            config::revision(&config),
            &config.bridge_id,
            config::paired(),
            &local_storage,
            peer,
        ) {
            continue;
        }
        let Ok(remote) = fetch(peer.addr).await else {
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
    publish_shared(shared, &config)?;
    *conflict = peers.values().any(|peer| {
        peer.paired
            && config::paired()
            && !peer.bridge.is_empty()
            && !config.bridge_id.is_empty()
            && peer.bridge != config.bridge_id
    });

    let views: Vec<PeerView> = peers
        .values()
        .filter(|peer| {
            peer.bridge.is_empty() || config.bridge_id.is_empty() || peer.bridge == config.bridge_id
        })
        .map(|peer| peer.view.clone())
        .collect();
    let decided = role(id, *i_lead, &views, now, started);
    let signature = signature(&config);
    match decided {
        Role::Serve => {
            let restart = serving
                .as_ref()
                .is_none_or(|server| server.signature != signature);
            if restart {
                stop_server(serving).await;
                *serving = Some(spawn_server(config, signature).await?);
            }
            *i_lead = true;
            let role_name = if *conflict { "conflict" } else { "leader" };
            status::set_membership(role_name, name);
        }
        Role::Standby { leader } => {
            stop_server(serving).await;
            *i_lead = false;
            let role_name = if *conflict { "conflict" } else { "standby" };
            status::publish_standby(&config, &leader, role_name);
        }
        Role::Starting => {
            stop_server(serving).await;
            *i_lead = false;
            status::publish_standby(&config, "", "starting");
        }
    }
    Ok(())
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

async fn announce(udp: &UdpSocket, id: &str, name: &str, config: &Config, leader: bool) {
    let hello = Hello {
        v: 1,
        id: id.to_owned(),
        name: name.to_owned(),
        bridge: config.bridge_id.clone(),
        paired: config::paired(),
        config_rev: config::revision(config),
        storage: storage_hash(),
        leader,
    };
    let Ok(bytes) = serde_json::to_vec(&hello) else {
        return;
    };
    let _ = udp.send_to(&bytes, ("255.255.255.255", UDP_PORT)).await;
}

fn decode_hello(bytes: &[u8], addr: IpAddr) -> Option<Peer> {
    let hello: Hello = serde_json::from_slice(bytes).ok()?;
    if hello.v != 1 || hello.id.is_empty() {
        return None;
    }
    Some(Peer {
        view: PeerView {
            id: hello.id,
            name: hello.name,
            leader: hello.leader,
            seen_ms: now_ms(),
        },
        addr,
        bridge: hello.bridge,
        paired: hello.paired,
        config_rev: hello.config_rev,
        storage: hello.storage,
    })
}

struct Remote {
    config: Config,
    files: BTreeMap<String, Vec<u8>>,
}

async fn fetch(addr: IpAddr) -> Result<Remote> {
    let mut stream =
        tokio::time::timeout(Duration::from_secs(3), TcpStream::connect((addr, TCP_PORT)))
            .await
            .context("the other machine did not answer")?
            .context("could not reach the other machine")?;
    stream.write_all(b"state\n").await?;
    let mut bytes = Vec::new();
    stream.take(4_000_000).read_to_end(&mut bytes).await?;
    let envelope: Envelope = serde_json::from_slice(&bytes).context("bad sync reply")?;
    let config = config::config_from_toml(&envelope.config)?;
    let mut files = BTreeMap::new();
    for (name, encoded) in envelope.files {
        if name.contains("..") || name.starts_with('/') {
            continue;
        }
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) {
            files.insert(name, bytes);
        }
    }
    Ok(Remote { config, files })
}

async fn accept(listener: TcpListener, shared: Arc<Mutex<Vec<u8>>>) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            break;
        };
        let body = shared
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        tokio::spawn(async move {
            let mut probe = [0u8; 8];
            let _ = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut probe)).await;
            let _ = stream.write_all(&body).await;
            let _ = stream.shutdown().await;
        });
    }
}

fn publish_shared(shared: &Mutex<Vec<u8>>, config: &Config) -> Result<()> {
    let (hash, files) = storage_files();
    let envelope = Envelope {
        config: toml::to_string_pretty(config)?,
        storage: hash,
        files,
    };
    let bytes = serde_json::to_vec(&envelope)?;
    *shared.lock().unwrap_or_else(|poison| poison.into_inner()) = bytes;
    Ok(())
}

fn storage_files() -> (String, BTreeMap<String, String>) {
    let mut raw = BTreeMap::new();
    let Ok(dir) = config::homekit_dir() else {
        return ("0".into(), BTreeMap::new());
    };
    if walk(&dir, &dir, &mut raw).is_err() || raw.is_empty() {
        return ("0".into(), BTreeMap::new());
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
        return (hash, BTreeMap::new());
    }
    let encoded = raw
        .into_iter()
        .map(|(name, bytes)| {
            (
                name,
                base64::engine::general_purpose::STANDARD.encode(bytes),
            )
        })
        .collect();
    (hash, encoded)
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
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
