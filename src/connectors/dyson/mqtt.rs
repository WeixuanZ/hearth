//! A small MQTT 3.1 client.
//!
//! Dyson fans speak protocol level 3 (`MQIsdp`), not MQTT 3.1.1. Most modern
//! client libraries only offer 3.1.1 and 5, and the fan refuses those.

use anyhow::{anyhow, Context, Result};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, ReadHalf},
    net::TcpStream,
    sync::{mpsc, Mutex},
    time::{interval, timeout},
};

#[derive(Debug, Clone)]
pub struct Publish {
    pub topic: String,
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    pub topic: String,
    pub payload: String,
}

pub fn encode_connect(client_id: &str, username: &str, password: &str, keepalive: u16) -> Vec<u8> {
    let mut variable = Vec::new();
    variable.extend(mqtt_string("MQIsdp"));
    variable.push(3);
    variable.push(0xC2); // username, password, clean session
    variable.extend(keepalive.to_be_bytes());
    let mut payload = Vec::new();
    payload.extend(mqtt_string(client_id));
    payload.extend(mqtt_string(username));
    payload.extend(mqtt_string(password));
    packet(0x10, &[variable, payload].concat())
}

pub fn encode_subscribe(packet_id: u16, topic: &str) -> Vec<u8> {
    let mut body = packet_id.to_be_bytes().to_vec();
    body.extend(mqtt_string(topic));
    body.push(0);
    packet(0x82, &body)
}

pub fn encode_publish(packet_id: u16, topic: &str, payload: &str) -> Vec<u8> {
    let mut body = mqtt_string(topic);
    body.extend(packet_id.to_be_bytes());
    body.extend(payload.as_bytes());
    packet(0x32, &body)
}

fn mqtt_string(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = (bytes.len() as u16).to_be_bytes().to_vec();
    out.extend(bytes);
    out
}

fn packet(header: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![header];
    out.extend(remaining_length(body.len()));
    out.extend(body);
    out
}

fn remaining_length(mut len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut encoded = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            encoded |= 0x80;
        }
        out.push(encoded);
        if len == 0 {
            break;
        }
    }
    out
}

async fn read_packet(reader: &mut ReadHalf<TcpStream>) -> Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 1];
    reader.read_exact(&mut header).await.context("mqtt read")?;
    let mut multiplier = 1usize;
    let mut length = 0usize;
    for _ in 0..4 {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).await.context("mqtt length")?;
        length += (byte[0] & 0x7F) as usize * multiplier;
        if byte[0] & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
        if multiplier > 128 * 128 * 128 {
            return Err(anyhow!("mqtt remaining length is too long"));
        }
    }
    if length > 1_000_000 {
        return Err(anyhow!("mqtt packet is unexpectedly large"));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await.context("mqtt body")?;
    Ok((header[0], body))
}

fn parse_publish(header: u8, body: &[u8]) -> Result<(u8, Option<u16>, Incoming)> {
    if body.len() < 2 {
        return Err(anyhow!("mqtt publish is truncated"));
    }
    let topic_len = u16::from_be_bytes([body[0], body[1]]) as usize;
    let mut index = 2 + topic_len;
    if body.len() < index {
        return Err(anyhow!("mqtt publish topic is truncated"));
    }
    let topic = String::from_utf8(body[2..index].to_vec()).context("mqtt topic")?;
    let qos = (header >> 1) & 0x03;
    let packet_id = if qos > 0 {
        if body.len() < index + 2 {
            return Err(anyhow!("mqtt publish is missing a packet id"));
        }
        let id = u16::from_be_bytes([body[index], body[index + 1]]);
        index += 2;
        Some(id)
    } else {
        None
    };
    let payload = String::from_utf8_lossy(&body[index..]).into_owned();
    Ok((qos, packet_id, Incoming { topic, payload }))
}

/// Where an MQTT broker is, and how to log in to it.
pub struct Broker<'a> {
    pub host: &'a str,
    pub port: u16,
    pub username: &'a str,
    pub password: &'a str,
}

/// Hold one connection open: subscribed to `topics`, with `hello` sent once
/// subscribed. Returns when the socket dies; the caller reconnects.
pub async fn session(
    broker: &Broker<'_>,
    topics: &[String],
    hello: &[Publish],
    commands: &mut mpsc::Receiver<Publish>,
    events: &mpsc::Sender<Incoming>,
) -> Result<()> {
    let Broker {
        host,
        port,
        username,
        password,
    } = *broker;
    let stream = timeout(Duration::from_secs(8), TcpStream::connect((host, port)))
        .await
        .context("timed out reaching the purifier")?
        .with_context(|| format!("cannot reach the purifier at {host}:{port}"))?;
    stream.set_nodelay(true).ok();
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = std::sync::Arc::new(Mutex::new(writer));

    {
        // The broker drops a client when another logs in with the same id.
        // Using the serial, as the Dyson app does, let the app knock the
        // bridge off. A fresh id per connection avoids that.
        let client_id = format!("hearth-{:08x}", rand::random::<u32>());
        let mut write = writer.lock().await;
        write
            .write_all(&encode_connect(&client_id, username, password, 30))
            .await
            .context("mqtt connect")?;
        write.flush().await.ok();
    }
    let (header, body) = timeout(Duration::from_secs(8), read_packet(&mut reader))
        .await
        .context("the purifier did not answer the MQTT handshake")??;
    if header >> 4 != 2 || body.len() < 2 || body[1] != 0 {
        let code = body.get(1).copied().unwrap_or(0xFF);
        return Err(anyhow!(
            "the purifier refused MQTT (code {code}). The serial or local password is wrong, or another app is holding the only connection"
        ));
    }

    let mut packet_id = 1u16;
    {
        let mut write = writer.lock().await;
        for topic in topics {
            write.write_all(&encode_subscribe(packet_id, topic)).await?;
            packet_id = packet_id.wrapping_add(1).max(1);
            write.flush().await.ok();
        }
        for publish in hello {
            write
                .write_all(&encode_publish(packet_id, &publish.topic, &publish.payload))
                .await?;
            packet_id = packet_id.wrapping_add(1).max(1);
        }
        write.flush().await.ok();
    }

    let ping_writer = writer.clone();
    let ping = tokio::spawn(async move {
        let mut ticks = interval(Duration::from_secs(20));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            let mut write = ping_writer.lock().await;
            if write.write_all(&[0xC0, 0x00]).await.is_err() {
                break;
            }
            write.flush().await.ok();
        }
    });

    let result = loop {
        tokio::select! {
            // The pings draw an answer every 20 seconds, so a minute of
            // silence is a link that died without closing, as when the fan
            // drops off the network. Ending lets the caller reconnect.
            packet = timeout(Duration::from_secs(60), read_packet(&mut reader)) => {
                let (header, body) = match packet {
                    Ok(Ok(packet)) => packet,
                    Ok(Err(error)) => break Err(error),
                    Err(_) => break Err(anyhow!("the purifier went quiet")),
                };
                match header >> 4 {
                    3 => {
                        let (qos, id, incoming) = parse_publish(header, &body)?;
                        if qos == 1 {
                            if let Some(id) = id {
                                let mut write = writer.lock().await;
                                let mut ack = vec![0x40, 0x02];
                                ack.extend(id.to_be_bytes());
                                write.write_all(&ack).await.ok();
                            }
                        }
                        if events.send(incoming).await.is_err() {
                            break Ok(());
                        }
                    }
                    13 | 9 | 4 => {}
                    14 => break Err(anyhow!("the purifier closed the MQTT connection")),
                    _ => {}
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { break Ok(()) };
                let bytes = encode_publish(packet_id, &command.topic, &command.payload);
                packet_id = packet_id.wrapping_add(1).max(1);
                let mut write = writer.lock().await;
                if let Err(error) = write.write_all(&bytes).await {
                    break Err(error.into());
                }
                write.flush().await.ok();
            }
        }
    };
    ping.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_packet_is_mqtt_3_1() {
        let packet = encode_connect("SN", "SN", "pw", 30);
        assert_eq!(packet[0], 0x10);
        let text = String::from_utf8_lossy(&packet);
        assert!(text.contains("MQIsdp"));
        assert!(!text.contains("MQTT"));
        assert!(packet.windows(2).any(|pair| pair == [3, 0xC2]));
    }

    #[test]
    fn publish_round_trip_header() {
        let packet = encode_publish(7, "527K/SN/command", r#"{"msg":"STATE-SET"}"#);
        assert_eq!(packet[0], 0x32);
        let (qos, id, incoming) = parse_publish(packet[0], &packet[2..]).unwrap();
        assert_eq!(qos, 1);
        assert_eq!(id, Some(7));
        assert_eq!(incoming.topic, "527K/SN/command");
        assert!(incoming.payload.contains("STATE-SET"));
    }
}
