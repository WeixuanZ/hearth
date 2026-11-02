//! `hearth watch`: what is on the local network, and what changed since
//! hearth last looked. Each device is known by its hardware address, with
//! the address it has now, its name and services on mDNS, and which common
//! ports it answers on. A new device, a port newly open, or a gateway that
//! answers from another device's hardware address (ARP spoofing, someone
//! putting themselves between this network and the internet) is told.
//!
//! What it has seen is kept on this computer only, in `network.json`:
//! every device seen in the last 30 days, with every port and service it
//! has shown. Phones and laptops sleep, close ports and leave all day, so
//! only what was never seen before is told, not what comes and goes.

mod scan;

use crate::config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

/// The layout of `network.json`. A newer one is left alone.
const FORMAT: u32 = 1;

/// A device not seen for this long is forgotten.
const FORGET: i64 = 30 * 24 * 3600;

/// One device on the network.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Host {
    /// Its hardware address, lowercase, as `00:11:22:33:44:55`.
    pub mac: String,
    /// The addresses it answers for: one, for anything but a router or a
    /// device passing itself off as another.
    pub ips: BTreeSet<Ipv4Addr>,
    /// The name it gives on mDNS, if any.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub ports: BTreeSet<u16>,
    /// The mDNS services it offers, like `_airplay._tcp`.
    #[serde(default)]
    pub services: BTreeSet<String>,
    /// When it was last seen, as seconds since 1970.
    #[serde(default)]
    pub seen: i64,
}

/// The network as seen at one look, or as remembered from all of them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Network {
    pub format: u32,
    /// When, as seconds since 1970.
    pub seen: i64,
    pub gateway: Option<Ipv4Addr>,
    /// The hardware address the gateway answered from.
    #[serde(default)]
    pub gateway_mac: Option<String>,
    /// Every device, by hardware address.
    pub hosts: BTreeMap<String, Host>,
}

impl Network {
    /// The device answering for the gateway, in one look.
    fn gateway_host(&self) -> Option<&Host> {
        let gateway = self.gateway?;
        self.hosts.values().find(|host| host.ips.contains(&gateway))
    }

    /// What is remembered after `look`: every device in it, with whatever
    /// else this one had seen of it; and the others seen in the last 30 days.
    fn remember(&self, look: &Network) -> Network {
        let mut hosts = look.hosts.clone();
        for (mac, host) in &mut hosts {
            if let Some(was) = self.hosts.get(mac) {
                host.ports.extend(was.ports.iter().copied());
                host.services.extend(was.services.iter().cloned());
                if host.name.is_empty() {
                    host.name.clone_from(&was.name);
                }
            }
        }
        for (mac, was) in &self.hosts {
            if !hosts.contains_key(mac) && look.seen - was.seen < FORGET {
                hosts.insert(mac.clone(), was.clone());
            }
        }
        Network {
            hosts,
            ..look.clone()
        }
    }
}

impl Host {
    /// Where to reach it: its first address.
    pub fn ip(&self) -> Option<Ipv4Addr> {
        self.ips.iter().next().copied()
    }
}

/// What is different from the last look.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    Appeared {
        host: Host,
    },
    Moved {
        mac: String,
        from: Vec<Ipv4Addr>,
        to: Vec<Ipv4Addr>,
    },
    Opened {
        host: Host,
        ports: Vec<u16>,
    },
    Offers {
        host: Host,
        services: Vec<String>,
    },
    /// The gateway answers from another hardware address than before.
    GatewayChanged {
        gateway: Ipv4Addr,
        from: String,
        to: String,
    },
    /// The gateway's hardware address is also another device's: that
    /// device may be passing itself off as the gateway.
    Spoofing {
        gateway: Ipv4Addr,
        mac: String,
        also: Vec<Ipv4Addr>,
    },
}

impl Change {
    /// Worth a look at once, not just a note.
    pub fn alarming(&self) -> bool {
        matches!(
            self,
            Self::Appeared { .. }
                | Self::Opened { .. }
                | Self::GatewayChanged { .. }
                | Self::Spoofing { .. }
        )
    }
}

fn addresses(ips: &[Ipv4Addr]) -> String {
    if ips.is_empty() {
        return "none".into();
    }
    let words: Vec<String> = ips.iter().map(Ipv4Addr::to_string).collect();
    words.join(", ")
}

fn label(host: &Host) -> String {
    let ips: Vec<Ipv4Addr> = host.ips.iter().copied().collect();
    let at = addresses(&ips);
    if host.name.is_empty() {
        format!("{at} ({})", host.mac)
    } else {
        format!("{} at {at} ({})", host.name, host.mac)
    }
}

fn ports(ports: &[u16]) -> String {
    let words: Vec<String> = ports.iter().map(u16::to_string).collect();
    words.join(", ")
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Appeared { host } => write!(f, "new device: {}", label(host)),
            Self::Moved { mac, from, to } => {
                write!(f, "{mac} moved from {} to {}", addresses(from), addresses(to))
            }
            Self::Opened { host, ports: open } => write!(f, "{} now answers on port {}", label(host), ports(open)),
            Self::Offers { host, services } => write!(f, "{} now offers {}", label(host), services.join(", ")),
            Self::GatewayChanged { gateway, from, to } => write!(
                f,
                "the gateway {gateway} answers from {to}, not {from}: a new router, or someone in between"
            ),
            Self::Spoofing { gateway, mac, also } => write!(
                f,
                "{} answers as the gateway {gateway} with {mac}: it may be passing itself off as the router",
                addresses(also)
            ),
        }
    }
}

/// What is new in `after`, a look, beside what was remembered `before`: a
/// device never seen, one at a new address, a port or a service it never
/// showed, and a gateway answering from another device.
pub fn changes(before: &Network, after: &Network) -> Vec<Change> {
    let mut found = Vec::new();
    for (mac, host) in &after.hosts {
        let Some(was) = before.hosts.get(mac) else {
            found.push(Change::Appeared { host: host.clone() });
            continue;
        };
        if was.ips != host.ips {
            found.push(Change::Moved {
                mac: mac.clone(),
                from: was.ips.iter().copied().collect(),
                to: host.ips.iter().copied().collect(),
            });
        }
        let opened: Vec<u16> = host.ports.difference(&was.ports).copied().collect();
        if !opened.is_empty() {
            found.push(Change::Opened {
                host: host.clone(),
                ports: opened,
            });
        }
        let offers: Vec<String> = host.services.difference(&was.services).cloned().collect();
        if !offers.is_empty() {
            found.push(Change::Offers {
                host: host.clone(),
                services: offers,
            });
        }
    }
    if let (Some(gateway), Some(was), Some(now)) =
        (after.gateway, &before.gateway_mac, &after.gateway_mac)
    {
        if before.gateway == after.gateway && was != now {
            found.push(Change::GatewayChanged {
                gateway,
                from: was.clone(),
                to: now.clone(),
            });
        }
    }
    found.extend(spoofing(after));
    found
}

/// Another address answering with the gateway's hardware address. Seen in
/// one look, not by comparing two, so it is caught on the first look too.
fn spoofing(network: &Network) -> Option<Change> {
    let gateway = network.gateway?;
    let host = network.gateway_host()?;
    let also: Vec<Ipv4Addr> = host
        .ips
        .iter()
        .copied()
        .filter(|ip| *ip != gateway)
        .collect();
    (!also.is_empty()).then(|| Change::Spoofing {
        gateway,
        mac: host.mac.clone(),
        also,
    })
}

fn path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("network.json"))
}

/// What was seen last, if anything, in a format this hearth reads.
fn load(path: &Path) -> Result<Option<Network>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let network: Network = serde_json::from_str(&text).context("reading network.json")?;
    Ok((network.format == FORMAT).then_some(network))
}

fn save(path: &Path, network: &Network) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let next = path.with_extension("json.next");
    std::fs::write(&next, serde_json::to_vec_pretty(network)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(next, path)?;
    Ok(())
}

/// Looks at the network, tells what changed since last time, and keeps
/// this look for the next. With `every`, looks again that many minutes
/// later, for as long as it runs, telling only what changed.
pub async fn run(every: Option<u64>, as_json: bool) -> Result<()> {
    let path = path()?;
    let mut first = true;
    loop {
        let before = load(&path)?;
        let now = scan::look().await?;
        let found = before
            .as_ref()
            .map(|before| changes(before, &now))
            .unwrap_or_else(|| {
                // Nothing to compare with: only what is wrong in itself.
                spoofing(&now).into_iter().collect()
            });
        if as_json {
            let shown = serde_json::json!({
                "network": if first { Some(&now) } else { None },
                "changes": found,
                "first_look": before.is_none(),
            });
            println!("{shown}");
        } else {
            if first {
                print_network(&now);
            }
            print_changes(&found, before.as_ref());
        }
        let remembered = before
            .map(|before| before.remember(&now))
            .unwrap_or_else(|| now.clone());
        save(&path, &remembered)?;
        first = false;
        let Some(minutes) = every else { break };
        tokio::time::sleep(std::time::Duration::from_secs(minutes.max(1) * 60)).await;
    }
    Ok(())
}

fn print_network(network: &Network) {
    let gateway = network
        .gateway
        .map(|ip| format!(", gateway {ip}"))
        .unwrap_or_default();
    println!("{} devices{gateway}", network.hosts.len());
    let mut hosts: Vec<&Host> = network.hosts.values().collect();
    hosts.sort_by_key(|host| host.ip());
    for host in hosts {
        let ip = host
            .ip()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "?".into());
        let mut line = format!("  {ip:<15}  {}", host.mac);
        if !host.name.is_empty() {
            line += &format!("  {}", host.name);
        }
        if !host.ports.is_empty() {
            let open: Vec<u16> = host.ports.iter().copied().collect();
            line += &format!("  ports {}", ports(&open));
        }
        if !host.services.is_empty() {
            let services: Vec<&str> = host.services.iter().map(String::as_str).collect();
            line += &format!("  {}", services.join(" "));
        }
        println!("{line}");
    }
}

fn print_changes(found: &[Change], before: Option<&Network>) {
    match before {
        None if found.is_empty() => println!("First look: kept, to compare with next time."),
        None => println!("First look: kept, to compare with next time. Already:"),
        Some(before) if found.is_empty() => {
            println!("Nothing changed since {}.", when(before.seen));
            return;
        }
        Some(before) => println!("Since {}:", when(before.seen)),
    }
    for change in found {
        let mark = if change.alarming() { "!" } else { "-" };
        println!("  {mark} {change}");
    }
}

fn when(seen: i64) -> String {
    chrono::DateTime::from_timestamp(seen, 0)
        .map(|at| {
            at.with_timezone(&chrono::Local)
                .format("%-d %b %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "the last look".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(ip: [u8; 4], mac: &str, ports: &[u16]) -> Host {
        Host {
            ips: BTreeSet::from([Ipv4Addr::from(ip)]),
            mac: mac.into(),
            ports: ports.iter().copied().collect(),
            ..Host::default()
        }
    }

    fn network(hosts: &[Host]) -> Network {
        let mut network = Network {
            format: FORMAT,
            seen: 0,
            gateway: Some(Ipv4Addr::new(10, 0, 0, 1)),
            gateway_mac: None,
            hosts: hosts
                .iter()
                .map(|host| (host.mac.clone(), host.clone()))
                .collect(),
        };
        network.gateway_mac = network.gateway_host().map(|host| host.mac.clone());
        network
    }

    const ROUTER: &str = "00:00:5e:00:53:01";
    const LAPTOP: &str = "00:00:5e:00:53:02";
    const CAMERA: &str = "00:00:5e:00:53:03";

    #[test]
    fn a_new_device_and_a_newly_open_port_are_told() {
        let before = network(&[
            host([10, 0, 0, 1], ROUTER, &[53, 80]),
            host([10, 0, 0, 2], LAPTOP, &[]),
        ]);
        let after = network(&[
            host([10, 0, 0, 1], ROUTER, &[53, 80]),
            host([10, 0, 0, 5], LAPTOP, &[22]),
            host([10, 0, 0, 9], CAMERA, &[554]),
        ]);
        let found = changes(&before, &after);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(matches!(&found[0], Change::Moved { mac, .. } if mac == LAPTOP));
        assert!(matches!(&found[1], Change::Opened { ports, .. } if ports == &[22]));
        assert!(matches!(&found[2], Change::Appeared { host } if host.mac == CAMERA));
        assert!(found[2].alarming());
        assert_eq!(
            found[2].to_string(),
            "new device: 10.0.0.9 (00:00:5e:00:53:03)"
        );
    }

    #[test]
    fn what_comes_and_goes_is_not_news() {
        let first = network(&[
            host([10, 0, 0, 1], ROUTER, &[53]),
            host([10, 0, 0, 2], LAPTOP, &[22, 62078]),
        ]);
        // The laptop asleep: gone, then back with fewer ports open.
        let mut asleep = network(&[host([10, 0, 0, 1], ROUTER, &[53])]);
        asleep.seen = 3600;
        assert!(changes(&first, &asleep).is_empty());
        let remembered = first.remember(&asleep);
        assert!(remembered.hosts.contains_key(LAPTOP));
        let awake = network(&[
            host([10, 0, 0, 1], ROUTER, &[53]),
            host([10, 0, 0, 2], LAPTOP, &[22]),
        ]);
        assert!(changes(&remembered, &awake).is_empty());
        let remembered = remembered.remember(&awake);
        assert_eq!(remembered.hosts[LAPTOP].ports, BTreeSet::from([22, 62078]));
        // A month later it is forgotten, so it would be news again.
        let mut later = network(&[host([10, 0, 0, 1], ROUTER, &[53])]);
        later.seen = FORGET + 1;
        assert!(!remembered.remember(&later).hosts.contains_key(LAPTOP));
    }

    #[test]
    fn a_device_answering_as_the_gateway_is_spoofing() {
        let before = network(&[
            host([10, 0, 0, 1], ROUTER, &[]),
            host([10, 0, 0, 2], LAPTOP, &[]),
        ]);
        // The laptop's hardware address now answers for the gateway too.
        let mut laptop = host([10, 0, 0, 2], LAPTOP, &[]);
        laptop.ips.insert(Ipv4Addr::new(10, 0, 0, 1));
        let after = network(&[laptop]);
        let found = changes(&before, &after);
        assert!(
            found
                .iter()
                .any(|change| matches!(change, Change::GatewayChanged { to, .. } if to == LAPTOP)),
            "{found:?}"
        );
        let spoofing = found
            .iter()
            .find(|change| matches!(change, Change::Spoofing { .. }))
            .unwrap();
        assert!(spoofing.alarming());
        assert!(spoofing
            .to_string()
            .starts_with("10.0.0.2 answers as the gateway 10.0.0.1"));
    }
}
