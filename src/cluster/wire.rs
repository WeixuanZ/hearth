//! One frame for every message between hearth processes.
//!
//! ```text
//! version: 1 byte
//! kind:    1 byte
//! length:  4 bytes, big endian
//! body:    UTF-8 JSON, or ciphertext of JSON
//! ```
//!
//! JSON is serde. There is no hand-parsed text. File bytes ride inside the
//! JSON as base64 because a HomeKit directory is small. The frame length is
//! what stops a peer from being read forever.

use anyhow::{anyhow, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u8 = 1;
pub const HELLO: u8 = 1;
pub const JOIN: u8 = 2;
pub const JOIN_ACK: u8 = 3;
pub const GRANT: u8 = 4;
pub const DENY: u8 = 5;
pub const STATE_ASK: u8 = 6;
pub const STATE: u8 = 7;
pub const APPROVE: u8 = 8;
pub const DECLINE: u8 = 9;
pub const LEAVE: u8 = 10;
pub const START: u8 = 11;
pub const REPLY: u8 = 12;
pub const CONFIRM: u8 = 13;

const MAX_BODY: usize = 4_000_000;

#[derive(Debug)]
pub struct Frame {
    pub kind: u8,
    pub body: Vec<u8>,
}

pub fn encode(kind: u8, body: &[u8]) -> Result<Vec<u8>> {
    if body.len() > MAX_BODY {
        anyhow::bail!("message is too large");
    }
    let mut out = Vec::with_capacity(6 + body.len());
    out.push(VERSION);
    out.push(kind);
    out.extend(u32::try_from(body.len()).expect("capped").to_be_bytes());
    out.extend(body);
    Ok(out)
}

pub fn decode(bytes: &[u8]) -> Result<Frame> {
    if bytes.len() < 6 {
        anyhow::bail!("short message");
    }
    if bytes[0] != VERSION {
        anyhow::bail!("unsupported hearth protocol");
    }
    let len = u32::from_be_bytes(bytes[2..6].try_into().expect("four bytes")) as usize;
    if len > MAX_BODY || bytes.len() != 6 + len {
        anyhow::bail!("bad message length");
    }
    Ok(Frame {
        kind: bytes[1],
        body: bytes[6..].to_vec(),
    })
}

pub async fn write_frame<W>(io: &mut W, kind: u8, body: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let bytes = encode(kind, body)?;
    io.write_all(&bytes).await?;
    io.flush().await?;
    Ok(())
}

pub async fn read_frame<R>(io: &mut R) -> Result<Frame>
where
    R: AsyncRead + Unpin,
{
    let mut head = [0u8; 6];
    io.read_exact(&mut head)
        .await
        .context("the connection closed")?;
    if head[0] != VERSION {
        anyhow::bail!("unsupported hearth protocol");
    }
    let len = u32::from_be_bytes(head[2..6].try_into().expect("four bytes")) as usize;
    if len > MAX_BODY {
        anyhow::bail!("message is too large");
    }
    let mut body = vec![0u8; len];
    io.read_exact(&mut body)
        .await
        .context("the connection closed")?;
    Ok(Frame {
        kind: head[1],
        body,
    })
}

pub fn to_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}

pub fn from_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|error| anyhow!("could not read the message: {error}"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub cluster: String,
    #[serde(default)]
    pub bridge: String,
    #[serde(default)]
    pub paired: bool,
    #[serde(default)]
    pub config_rev: u64,
    #[serde(default)]
    pub storage: String,
    #[serde(default)]
    pub leader: bool,
    #[serde(default)]
    pub mac: String,
}

impl Hello {
    /// Fixed layout. JSON object order is not what the check is computed over.
    pub fn material(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for part in [
            &self.id,
            &self.name,
            &self.cluster,
            &self.bridge,
            &self.storage,
        ] {
            let len = u32::try_from(part.len()).unwrap_or(u32::MAX);
            out.extend(len.to_be_bytes());
            out.extend(part.as_bytes());
        }
        out.push(u8::from(self.paired));
        out.extend(self.config_rev.to_be_bytes());
        out.push(u8::from(self.leader));
        out
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Handshake {
    pub id: String,
    pub name: String,
    pub eph: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Secret {
    pub id: String,
    pub key: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Ask {
    pub id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StateBody {
    pub config: String,
    pub storage: String,
    pub files: Vec<FileBlob>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileBlob {
    pub name: String,
    pub bytes: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Approve {
    pub code: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default)]
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_frame_round_trips_and_rejects_a_short_header() {
        let bytes = encode(HELLO, br#"{"id":"a"}"#).unwrap();
        let frame = decode(&bytes).unwrap();
        assert_eq!(frame.kind, HELLO);
        assert_eq!(frame.body, br#"{"id":"a"}"#);
        assert!(decode(&bytes[..5]).is_err());

        let (mut client, mut server) = tokio::io::duplex(64);
        write_frame(&mut client, JOIN, b"{}").await.unwrap();
        let got = read_frame(&mut server).await.unwrap();
        assert_eq!(got.kind, JOIN);
    }

    #[test]
    fn hello_json_uses_serde_and_keeps_unknown_fields_out_of_the_check() {
        let hello = Hello {
            id: "n".into(),
            name: "Mac".into(),
            cluster: "abc".into(),
            bridge: "hearth-1".into(),
            paired: true,
            config_rev: 4,
            storage: "ff".into(),
            leader: false,
            mac: "ignored".into(),
        };
        let bytes = to_json(&hello).unwrap();
        let parsed: Hello = from_json(&bytes).unwrap();
        assert_eq!(parsed, hello);
        let mut flipped = parsed.clone();
        flipped.leader = true;
        assert_ne!(parsed.material(), flipped.material());
    }
}
