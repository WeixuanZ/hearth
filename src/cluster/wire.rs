//! What hearth processes on one network say to each other.
//!
//! Each computer sends a [`Hello`] to everyone over UDP every second.
//! Anything else is one TCP connection per exchange, of [`Message`]s, each a
//! four-byte big-endian length and then JSON. The answering side opens with
//! a nonce. Between members, a request is sealed with the group key bound to
//! that nonce, so a recorded request cannot be played again.

use super::crypto::{self, Key};
use crate::config::Config;
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The HomeKit files are a few kilobytes. This stops a peer from making us
/// read forever.
const LIMIT: usize = 4 << 20;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub id: String,
    /// The computer's name, as people know it.
    pub name: String,
    /// Where this computer takes TCP connections.
    pub port: u16,
    /// The group's id. Empty before this computer has one.
    pub group: String,
    pub bridge: String,
    pub paired: bool,
    /// The config's newest date. See `config::revision`.
    pub revision: u64,
    /// A hash of the HomeKit files.
    pub storage: String,
    pub serving: bool,
    /// Milliseconds since 1970, so an old hello cannot be replayed.
    pub sent: u64,
    /// The rest, signed with the group key. Empty before a group.
    #[serde(default)]
    pub tag: String,
}

impl Hello {
    /// Everything but the tag, as signed.
    fn signed(&self) -> Vec<u8> {
        let unsigned = Hello {
            tag: String::new(),
            ..self.clone()
        };
        serde_json::to_vec(&unsigned).expect("a hello is plain data")
    }

    pub fn sign(&mut self, key: &Key) {
        self.tag = crypto::encode(&crypto::tag(key, &self.signed()));
    }

    pub fn verify(&self, key: &Key) -> bool {
        crypto::decode(&self.tag).is_ok_and(|tag| crypto::tag_ok(key, &self.signed(), &tag))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// The answering side's nonce for this connection.
    Nonce {
        nonce: String,
    },
    /// A computer asks to join, with its half of the key agreement.
    Join {
        id: String,
        name: String,
        public: String,
    },
    /// A member answers with its half. Both now show the same code.
    Joining {
        id: String,
        name: String,
        public: String,
    },
    /// The group key, sealed with the agreed secret.
    Granted {
        sealed: String,
    },
    Refused {
        reason: String,
    },
    /// A [`Request`] or its reply, sealed with the group key.
    Sealed {
        sealed: String,
    },
}

/// What one member asks another, inside [`Message::Sealed`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Its config and HomeKit files.
    State,
    /// A control socket request, for the computer serving Home.
    Control { request: Value },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct State {
    pub config: Config,
    pub paired: bool,
    /// The HomeKit directory: paths and base64 contents.
    pub files: BTreeMap<String, String>,
}

pub async fn send(io: &mut (impl AsyncWrite + Unpin), message: &impl Serialize) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    anyhow::ensure!(body.len() <= LIMIT, "a message too large to send");
    io.write_all(&(body.len() as u32).to_be_bytes()).await?;
    io.write_all(&body).await?;
    io.flush().await?;
    Ok(())
}

pub async fn receive<T: DeserializeOwned>(io: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    let mut length = [0u8; 4];
    io.read_exact(&mut length)
        .await
        .context("the other computer hung up")?;
    let length = u32::from_be_bytes(length) as usize;
    anyhow::ensure!(length <= LIMIT, "a message too large to read");
    let mut body = vec![0u8; length];
    io.read_exact(&mut body)
        .await
        .context("the other computer hung up")?;
    serde_json::from_slice(&body).context("a message that could not be read")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_round_trip_and_a_hello_is_signed() {
        let (mut one, mut other) = tokio::io::duplex(256);
        send(
            &mut one,
            &Message::Refused {
                reason: "busy".into(),
            },
        )
        .await
        .unwrap();
        let got: Message = receive(&mut other).await.unwrap();
        assert!(matches!(got, Message::Refused { reason } if reason == "busy"));

        let key = crypto::GroupKey::new().hello();
        let mut hello = Hello {
            id: "abc".into(),
            serving: false,
            ..Default::default()
        };
        hello.sign(&key);
        assert!(hello.verify(&key));
        hello.serving = true;
        assert!(!hello.verify(&key));
    }
}
