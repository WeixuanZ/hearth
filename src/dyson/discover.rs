//! Find the purifier on the LAN. Fans advertise `{productType}_{serial}` on
//! `_dyson_mqtt._tcp.local`.

use anyhow::{anyhow, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::{net::IpAddr, time::Duration};

pub async fn find_host(serial: &str, wait: Duration) -> Result<String> {
    let mdns = ServiceDaemon::new().map_err(|error| anyhow!("mDNS failed to start: {error}"))?;
    let receiver = mdns
        .browse("_dyson_mqtt._tcp.local.")
        .map_err(|error| anyhow!("mDNS browse failed: {error}"))?;
    let deadline = tokio::time::Instant::now() + wait;
    let wanted = serial.to_ascii_lowercase();

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let event = match tokio::time::timeout(remaining, receiver.recv_async()).await {
            Ok(Ok(event)) => event,
            _ => break,
        };
        let ServiceEvent::ServiceResolved(info) = event else {
            continue;
        };
        let fullname = info.get_fullname().to_ascii_lowercase();
        if !fullname.contains(&wanted) {
            continue;
        }
        let address = info
            .get_addresses()
            .iter()
            .find(|addr| matches!(addr, IpAddr::V4(_)))
            .or_else(|| info.get_addresses().iter().next())
            .map(ToString::to_string);
        if let Some(address) = address {
            let _ = mdns.shutdown();
            return Ok(address);
        }
    }
    let _ = mdns.shutdown();
    Err(anyhow!(
        "could not find {serial} on the network. Put its address in host in the config, and check this machine is on the same Wi-Fi"
    ))
}
