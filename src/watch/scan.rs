//! One look at the network: every address on this computer's /24 is sent a
//! packet, so the system learns who answers, then its neighbour table is
//! read, the gateway found, mDNS asked who offers what, and each device
//! tried on a few ports.

use super::{Host, Network, FORMAT};
use anyhow::{anyhow, Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpStream, sync::Semaphore, task::JoinSet};

/// Ports worth knowing are open: remote login and screens, the web, file
/// sharing and printing, cameras, and what smart devices and media use.
const PORTS: &[u16] = &[
    21, 22, 23, 53, 80, 139, 443, 445, 548, 554, 631, 1883, 3389, 5000, 5900, 7000, 8000, 8001,
    8002, 8008, 8009, 8080, 8443, 8883, 9100, 32400, 62078,
];

/// mDNS services asked for by name.
const SERVICES: &[&str] = &[
    "_http._tcp",
    "_https._tcp",
    "_ssh._tcp",
    "_smb._tcp",
    "_afpovertcp._tcp",
    "_rfb._tcp",
    "_airplay._tcp",
    "_raop._tcp",
    "_companion-link._tcp",
    "_hap._tcp",
    "_matter._tcp",
    "_googlecast._tcp",
    "_spotify-connect._tcp",
    "_sonos._tcp",
    "_ipp._tcp",
    "_printer._tcp",
    "_pdl-datastream._tcp",
    "_mqtt._tcp",
    "_dyson_mqtt._tcp",
    "_hue._tcp",
    "_esphomelib._tcp",
    "_amzn-wplay._tcp",
    "_workstation._tcp",
];

/// How long mDNS is listened to.
const LISTEN: Duration = Duration::from_secs(4);
/// How long a port has to answer.
const KNOCK: Duration = Duration::from_millis(600);
/// Ports tried at once.
const AT_ONCE: usize = 64;

pub async fn look() -> Result<Network> {
    let local = local_address()?;
    let (_, names) = tokio::join!(sweep(local), browse(LISTEN));
    let mut hosts: BTreeMap<String, Host> = BTreeMap::new();
    for (ip, mac) in neighbours().await? {
        if ip == local || !same_subnet(ip, local) {
            continue;
        }
        let host = hosts.entry(mac.clone()).or_insert_with(|| Host {
            mac,
            ..Host::default()
        });
        host.ips.insert(ip);
    }
    for host in hosts.values_mut() {
        for ip in &host.ips {
            if let Some((name, services)) = names.get(ip) {
                if host.name.is_empty() {
                    host.name.clone_from(name);
                }
                host.services.extend(services.iter().cloned());
            }
        }
    }
    let open = knock(hosts.values().filter_map(Host::ip).collect()).await;
    for host in hosts.values_mut() {
        if let Some(ports) = host.ip().and_then(|ip| open.get(&ip)) {
            host.ports.clone_from(ports);
        }
    }
    let seen = chrono::Utc::now().timestamp();
    for host in hosts.values_mut() {
        host.seen = seen;
    }
    let mut network = Network {
        format: FORMAT,
        seen,
        gateway: gateway().await,
        gateway_mac: None,
        hosts,
    };
    network.gateway_mac = network.gateway_host().map(|host| host.mac.clone());
    Ok(network)
}

/// This computer's address on the network the devices are on.
fn local_address() -> Result<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
    socket.connect("224.0.0.251:5353")?;
    match socket.local_addr()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Ok(ip),
        _ => Err(anyhow!("this computer is not on a local network")),
    }
}

fn same_subnet(ip: Ipv4Addr, local: Ipv4Addr) -> bool {
    ip.octets()[..3] == local.octets()[..3]
}

/// A packet to every other address on the /24, to the discard port: what
/// answers is put in the system's neighbour table, whether it replies or not.
async fn sweep(local: Ipv4Addr) {
    let Ok(socket) = tokio::net::UdpSocket::bind("0.0.0.0:0").await else {
        return;
    };
    let [a, b, c, _] = local.octets();
    for d in 1..=254u8 {
        let ip = Ipv4Addr::new(a, b, c, d);
        if ip != local {
            let _ = socket.send_to(&[0], SocketAddr::from((ip, 9))).await;
        }
    }
    // Time for the answers to the system's own neighbour requests.
    tokio::time::sleep(Duration::from_millis(1500)).await;
}

/// The system's neighbour table, as (address, hardware address).
async fn neighbours() -> Result<Vec<(Ipv4Addr, String)>> {
    if cfg!(target_os = "linux") {
        let table = std::fs::read_to_string("/proc/net/arp").context("reading /proc/net/arp")?;
        Ok(parse_proc_arp(&table))
    } else {
        let output = tokio::process::Command::new("arp")
            .arg("-an")
            .output()
            .await
            .context("running arp")?;
        Ok(parse_arp(&String::from_utf8_lossy(&output.stdout)))
    }
}

/// `arp -an`, as macOS and the BSDs print it:
/// `? (10.0.0.1) at 0:0:5e:0:53:1 on en0 ifscope [ethernet]`.
fn parse_arp(text: &str) -> Vec<(Ipv4Addr, String)> {
    text.lines()
        .filter_map(|line| {
            let ip = line.split_once('(')?.1.split_once(')')?.0.parse().ok()?;
            let mac = mac(line.split_once(" at ")?.1.split_whitespace().next()?)?;
            Some((ip, mac))
        })
        .collect()
}

/// Linux's `/proc/net/arp`: a header, then
/// `10.0.0.1  0x1  0x2  00:00:5e:00:53:01  *  eth0`. Flags 0x0 is not
/// resolved.
fn parse_proc_arp(text: &str) -> Vec<(Ipv4Addr, String)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.get(2) == Some(&"0x0") {
                return None;
            }
            Some((fields.first()?.parse().ok()?, mac(fields.get(3)?)?))
        })
        .collect()
}

/// A hardware address as `00:00:5e:00:53:01`, or None for one that is not
/// a device's: unresolved, broadcast, or multicast.
fn mac(text: &str) -> Option<String> {
    let parts: Vec<u8> = text
        .split(':')
        .map(|part| u8::from_str_radix(part, 16).ok())
        .collect::<Option<_>>()?;
    if parts.len() != 6 || parts.iter().all(|byte| *byte == 0) || parts[0] & 1 == 1 {
        return None;
    }
    let parts: Vec<String> = parts.iter().map(|byte| format!("{byte:02x}")).collect();
    Some(parts.join(":"))
}

/// The default route's gateway.
async fn gateway() -> Option<Ipv4Addr> {
    if cfg!(target_os = "linux") {
        parse_proc_route(&std::fs::read_to_string("/proc/net/route").ok()?)
    } else {
        let output = tokio::process::Command::new("route")
            .args(["-n", "get", "default"])
            .output()
            .await
            .ok()?;
        parse_route(&String::from_utf8_lossy(&output.stdout))
    }
}

/// `route -n get default` on macOS: a `gateway: 10.0.0.1` line.
fn parse_route(text: &str) -> Option<Ipv4Addr> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("gateway:"))
        .and_then(|gateway| gateway.trim().parse().ok())
}

/// `/proc/net/route`: the line whose destination is 00000000, with the
/// gateway in hex, least significant byte first.
fn parse_proc_route(text: &str) -> Option<Ipv4Addr> {
    text.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(1) != Some(&"00000000") {
            return None;
        }
        let gateway = u32::from_str_radix(fields.get(2)?, 16).ok()?;
        (gateway != 0).then(|| Ipv4Addr::from(gateway.swap_bytes()))
    })
}

/// Who answers on mDNS, by address: the name each gives, and the services
/// it offers, like `_airplay._tcp`.
async fn browse(listen: Duration) -> HashMap<Ipv4Addr, (String, BTreeSet<String>)> {
    let mut found: HashMap<Ipv4Addr, (String, BTreeSet<String>)> = HashMap::new();
    let Ok(mdns) = ServiceDaemon::new() else {
        return found;
    };
    let mut asked = JoinSet::new();
    for service in SERVICES {
        let Ok(receiver) = mdns.browse(&format!("{service}.local.")) else {
            continue;
        };
        asked.spawn(async move {
            let mut resolved = Vec::new();
            let deadline = tokio::time::Instant::now() + listen;
            while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, receiver.recv_async()).await
            {
                if let ServiceEvent::ServiceResolved(info) = event {
                    let name = info
                        .get_hostname()
                        .trim_end_matches('.')
                        .trim_end_matches(".local")
                        .to_owned();
                    for ip in info.get_addresses() {
                        if let IpAddr::V4(ip) = ip {
                            resolved.push((*ip, name.clone()));
                        }
                    }
                }
            }
            (*service, resolved)
        });
    }
    while let Some(Ok((service, resolved))) = asked.join_next().await {
        for (ip, name) in resolved {
            let entry = found.entry(ip).or_default();
            if entry.0.is_empty() {
                entry.0 = name;
            }
            entry.1.insert(service.to_owned());
        }
    }
    let _ = mdns.shutdown();
    found
}

async fn answers(ip: Ipv4Addr, port: u16, wait: Duration) -> bool {
    let address = SocketAddr::from((ip, port));
    matches!(
        tokio::time::timeout(wait, TcpStream::connect(address)).await,
        Ok(Ok(_))
    )
}

/// Which of `PORTS` each address answers on.
async fn knock(ips: Vec<Ipv4Addr>) -> HashMap<Ipv4Addr, BTreeSet<u16>> {
    let limit = Arc::new(Semaphore::new(AT_ONCE));
    let mut tries = JoinSet::new();
    for ip in ips {
        for &port in PORTS {
            let limit = limit.clone();
            tries.spawn(async move {
                let _turn = limit.acquire().await.ok()?;
                answers(ip, port, KNOCK).await.then_some((ip, port))
            });
        }
    }
    let mut open: HashMap<Ipv4Addr, BTreeSet<u16>> = HashMap::new();
    while let Some(result) = tries.join_next().await {
        if let Ok(Some((ip, port))) = result {
            open.entry(ip).or_default().insert(port);
        }
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_neighbour_table_as_macos_prints_it() {
        let table = "\
? (10.0.0.1) at 0:0:5e:0:53:1 on en0 ifscope [ethernet]
? (10.0.0.7) at (incomplete) on en0 ifscope [ethernet]
? (10.0.0.9) at 0:0:5e:0:53:9 on en0 ifscope [ethernet]
? (10.0.0.255) at ff:ff:ff:ff:ff:ff on en0 ifscope [ethernet]
? (224.0.0.251) at 1:0:5e:0:0:fb on en0 ifscope permanent [ethernet]
";
        assert_eq!(
            parse_arp(table),
            [
                (Ipv4Addr::new(10, 0, 0, 1), "00:00:5e:00:53:01".to_owned()),
                (Ipv4Addr::new(10, 0, 0, 9), "00:00:5e:00:53:09".to_owned()),
            ]
        );
    }

    #[test]
    fn reads_the_neighbour_table_as_linux_keeps_it() {
        let table = "\
IP address       HW type     Flags       HW address            Mask     Device
10.0.0.1         0x1         0x2         00:00:5e:00:53:01     *        eth0
10.0.0.7         0x1         0x0         00:00:00:00:00:00     *        eth0
";
        assert_eq!(
            parse_proc_arp(table),
            [(Ipv4Addr::new(10, 0, 0, 1), "00:00:5e:00:53:01".to_owned())]
        );
    }

    #[test]
    fn finds_the_gateway() {
        let macos =
            "   route to: default\ndestination: default\n    gateway: 10.0.0.1\n  interface: en0\n";
        assert_eq!(parse_route(macos), Some(Ipv4Addr::new(10, 0, 0, 1)));
        let linux = "\
Iface	Destination	Gateway 	Flags	RefCnt	Use	Metric	Mask		MTU	Window	IRTT
eth0	0000000A	00000000	0001	0	0	0	00FFFFFF	0	0	0
eth0	00000000	0100000A	0003	0	0	0	00000000	0	0	0
";
        assert_eq!(parse_proc_route(linux), Some(Ipv4Addr::new(10, 0, 0, 1)));
    }
}
