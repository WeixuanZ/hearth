//! Find a Samsung TV that answers the remote-control search.

use super::protocol::location_host;
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::net::UdpSocket;

const PROBES: &[&[u8]] = &[
    b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:samsung.com:device:RemoteControlReceiver:1\r\n\r\n",
    b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:dial-multiscreen-org:service:dial:1\r\n\r\n",
];

pub async fn search(wait: Duration) -> Result<Vec<String>> {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("could not search the network")?;
    socket.set_broadcast(true)?;
    for probe in PROBES {
        let _ = socket.send_to(probe, "239.255.255.250:1900").await;
    }
    let mut hosts = Vec::new();
    let deadline = tokio::time::Instant::now() + wait;
    let mut buffer = [0u8; 2048];
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(result) = tokio::time::timeout(remaining, socket.recv_from(&mut buffer)).await
        else {
            break;
        };
        let (size, _) = result?;
        let text = String::from_utf8_lossy(&buffer[..size]);
        if let Some(host) = location_host(&text) {
            if !hosts.iter().any(|known| known == &host) {
                hosts.push(host);
            }
        }
    }
    Ok(hosts)
}
