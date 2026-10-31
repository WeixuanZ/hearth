//! The dishwasher's local websocket.
//!
//! A current machine, the SPV2HKX42G among them, speaks JSON inside TLS-PSK
//! on port 443. An older one seals each frame itself and listens on port 80.
//! The key material saved at pairing says which.

use super::protocol::{decode_key, Command, Machine, Reading, Seal, Talk};
use crate::config::BoschDevice;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use openssl::ssl::{Ssl, SslContext, SslMethod, SslVerifyMode, SslVersion};
use std::{net::IpAddr, pin::Pin, time::Duration};
use tokio::{
    net::TcpStream,
    sync::{mpsc, watch},
};
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};

pub async fn session(
    device: &BoschDevice,
    states: Option<&watch::Sender<Reading>>,
    mut commands: Option<&mut mpsc::Receiver<Command>>,
) -> Result<Machine> {
    let mut link = connect(device).await?;
    let mut talk = Talk::new();
    let mut machine = states
        .map(|states| states.borrow().machine.clone())
        .unwrap_or_default();
    let mut poll = tokio::time::interval(Duration::from_secs(60));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    poll.tick().await;
    // The poll draws an answer every minute. Its own tick is not an answer,
    // so silence is timed from the last frame, not the last event.
    let mut heard = tokio::time::Instant::now();

    loop {
        let quiet = (heard + Duration::from_secs(150))
            .saturating_duration_since(tokio::time::Instant::now());
        let event =
            tokio::time::timeout(quiet, next(&mut link, &mut talk, &mut commands, &mut poll))
                .await
                .context("the dishwasher went quiet")?;
        match event {
            Event::Frame(frame) => {
                heard = tokio::time::Instant::now();
                let Some(text) = frame? else {
                    if talk.synced() {
                        return Ok(machine);
                    }
                    anyhow::bail!("the dishwasher closed the connection");
                };
                if text.is_empty() {
                    continue;
                }
                log::debug!("{} {}", device.name, resource_of(&text));
                for reply in talk.intake(&device.features, &mut machine, &text) {
                    link.send(&reply).await?;
                }
                if talk.started() {
                    publish(states, &machine);
                }
                if commands.is_none() && talk.synced() {
                    return Ok(machine);
                }
            }
            Event::Command(Some(command)) => {
                log::info!("{}: sending {command:?}", device.name);
                let starting = matches!(command, Command::Program(_) | Command::Power(true));
                if starting && talk.synced() && !machine.remote_start {
                    log::warn!(
                        "the dishwasher is not accepting remote start, so it may ignore this"
                    );
                }
                if let Some(text) = talk.submit(&device.features, command) {
                    link.send(&text).await?;
                }
            }
            Event::Command(None) => return Ok(machine),
            Event::Poll => {
                if let Some(text) = talk.poll() {
                    link.send(&text).await?;
                }
            }
        }
    }
}

enum Event {
    Frame(Result<Option<String>>),
    Command(Option<Command>),
    Poll,
}

async fn next(
    link: &mut Link,
    talk: &mut Talk,
    commands: &mut Option<&mut mpsc::Receiver<Command>>,
    poll: &mut tokio::time::Interval,
) -> Event {
    tokio::select! {
        biased;
        incoming = link.recv() => Event::Frame(incoming),
        command = next_command(commands), if talk.ready() => Event::Command(command),
        _ = poll.tick(), if talk.ready() => Event::Poll,
    }
}

async fn next_command(commands: &mut Option<&mut mpsc::Receiver<Command>>) -> Option<Command> {
    match commands.as_mut() {
        Some(commands) => commands.recv().await,
        None => std::future::pending().await,
    }
}

fn publish(states: Option<&watch::Sender<Reading>>, machine: &Machine) {
    let Some(states) = states else {
        return;
    };
    states
        .send(Reading {
            online: true,
            machine: machine.clone(),
        })
        .ok();
}

fn resource_of(text: &str) -> &str {
    let Some(start) = text.find("\"resource\"") else {
        return "frame";
    };
    let Some(colon) = text[start..].find(':') else {
        return "frame";
    };
    let rest = text[start + colon + 1..].trim_start_matches([' ', '"']);
    let end = rest.find('"').unwrap_or(rest.len());
    &rest[..end]
}

async fn connect(device: &BoschDevice) -> Result<Link> {
    let hosts = addresses(&device.host);
    if hosts.is_empty() {
        anyhow::bail!("the dishwasher has no address. Pair it again with --host");
    }
    let mut errors = Vec::new();
    for host in &hosts {
        match connect_host(host, device).await {
            Ok(link) => {
                log::info!("{} is at {host}", device.name);
                return Ok(link);
            }
            Err(error) => {
                log::debug!("{host}: {error:#}");
                errors.push(format!("{host}: {error:#}"));
            }
        }
    }
    anyhow::bail!("could not reach the dishwasher ({})", errors.join("; "))
}

async fn connect_host(host: &str, device: &BoschDevice) -> Result<Link> {
    let tls = device.iv.trim().is_empty();
    let port = if tls { 443 } else { 80 };
    let tcp = tokio::time::timeout(Duration::from_secs(8), TcpStream::connect((host, port)))
        .await
        .with_context(|| format!("timed out reaching {host}"))??;
    tcp.set_nodelay(true).ok();
    if tls {
        let psk = decode_key(&device.key)?;
        let ssl = client_ssl(host, &psk)?;
        let mut stream = tokio_openssl::SslStream::new(ssl, tcp)?;
        Pin::new(&mut stream).connect().await?;
        let (socket, _) =
            tokio_tungstenite::client_async(format!("wss://{host}/homeconnect"), stream).await?;
        Ok(Link {
            socket: Sock::Tls(socket),
            seal: None,
        })
    } else {
        let (socket, _) =
            tokio_tungstenite::client_async(format!("ws://{host}/homeconnect"), tcp).await?;
        Ok(Link {
            socket: Sock::Plain(socket),
            seal: Some(Seal::new(&device.key, &device.iv)?),
        })
    }
}

fn client_ssl(host: &str, psk: &[u8]) -> Result<Ssl> {
    let mut builder = SslContext::builder(SslMethod::tls_client())?;
    builder.set_cipher_list("ECDHE-PSK-CHACHA20-POLY1305")?;
    builder.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    builder.set_max_proto_version(Some(SslVersion::TLS1_2))?;
    builder.set_verify(SslVerifyMode::NONE);
    let psk = psk.to_vec();
    builder.set_psk_client_callback(move |_ssl, _hint, identity, key| {
        let name = b"HCCOM_Local_App\0";
        if identity.len() < name.len() || key.len() < psk.len() {
            return Err(openssl::error::ErrorStack::get());
        }
        identity[..name.len()].copy_from_slice(name);
        key[..psk.len()].copy_from_slice(&psk);
        Ok(psk.len())
    });
    let ctx = builder.build();
    let mut ssl = Ssl::new(&ctx)?;
    if host.parse::<IpAddr>().is_err() {
        let _ = ssl.set_hostname(host);
    }
    Ok(ssl)
}

fn addresses(host: &str) -> Vec<String> {
    let host = host.trim().trim_end_matches('.');
    let mut out = Vec::new();
    push_addr(&mut out, host);
    let ip = host.parse::<IpAddr>().is_ok();
    if !ip && !host.ends_with(".local") {
        push_addr(&mut out, &format!("{host}.local"));
    }
    let lower = host.to_ascii_lowercase();
    if lower != host {
        push_addr(&mut out, &lower);
        if !ip && !lower.ends_with(".local") {
            push_addr(&mut out, &format!("{lower}.local"));
        }
    }
    out
}

fn push_addr(out: &mut Vec<String>, host: &str) {
    if host.is_empty() || out.iter().any(|have| have.eq_ignore_ascii_case(host)) {
        return;
    }
    out.push(host.to_owned());
}

enum Sock {
    Tls(WebSocketStream<tokio_openssl::SslStream<TcpStream>>),
    Plain(WebSocketStream<TcpStream>),
}

struct Link {
    socket: Sock,
    seal: Option<Seal>,
}

impl Link {
    async fn send(&mut self, text: &str) -> Result<()> {
        let message = match self.seal.as_mut() {
            Some(seal) => Message::Binary(seal.seal(text.as_bytes()).into()),
            None => Message::Text(text.to_owned().into()),
        };
        self.send_raw(message).await
    }

    async fn send_raw(&mut self, message: Message) -> Result<()> {
        match &mut self.socket {
            Sock::Tls(socket) => socket.send(message).await?,
            Sock::Plain(socket) => socket.send(message).await?,
        }
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<String>> {
        let message = match &mut self.socket {
            Sock::Tls(socket) => socket.next().await,
            Sock::Plain(socket) => socket.next().await,
        };
        let Some(message) = message else {
            return Ok(None);
        };
        match message? {
            Message::Text(text) => Ok(Some(text.to_string())),
            Message::Binary(bytes) => {
                let plain = match self.seal.as_mut() {
                    Some(seal) => seal.open(bytes.as_ref())?,
                    None => bytes.to_vec(),
                };
                String::from_utf8(plain)
                    .context("the dishwasher sent a frame that was not text")
                    .map(Some)
            }
            Message::Ping(payload) => {
                self.send_raw(Message::Pong(payload)).await?;
                Ok(Some(String::new()))
            }
            Message::Close(_) => Ok(None),
            Message::Pong(_) => Ok(Some(String::new())),
            Message::Frame(_) => Ok(Some(String::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_tried_plain_and_as_a_local_name() {
        assert_eq!(
            addresses("BOSCH-Dishwasher-1"),
            vec![
                "BOSCH-Dishwasher-1".to_owned(),
                "BOSCH-Dishwasher-1.local".to_owned(),
            ]
        );
        assert_eq!(addresses("192.168.1.40"), vec!["192.168.1.40".to_owned()]);
    }

    #[test]
    fn tls_psk_accepts_the_dishwasher_cipher() {
        let key = decode_key("AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA").unwrap();
        client_ssl("BOSCH-Dishwasher-1", &key).unwrap();
    }
}
