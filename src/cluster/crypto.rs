//! The group key, and the handshake that hands it to a new computer.
//!
//! The two computers agree a secret with X25519 and both show a six-digit
//! code made from it. Someone types the new computer's code on the member,
//! and confirms on the new computer that they did. Anyone in between would
//! have agreed a different secret with each side, so the codes would differ.
//! The key then travels sealed with ChaCha20-Poly1305.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{fs, path::PathBuf};
use subtle::ConstantTimeEq;
use x25519_dalek::{EphemeralSecret, PublicKey};

pub type Key = [u8; 32];

/// One side of the key agreement.
pub struct Ephemeral {
    secret: EphemeralSecret,
    pub public: Key,
}

impl Ephemeral {
    pub fn new() -> Self {
        let secret = EphemeralSecret::random_from_rng(rand::thread_rng());
        let public = PublicKey::from(&secret).to_bytes();
        Self { secret, public }
    }

    pub fn agree(self, theirs: &Key) -> Key {
        self.secret
            .diffie_hellman(&PublicKey::from(*theirs))
            .to_bytes()
    }
}

/// The six digits both screens show, the same only with nobody in between.
pub fn code(shared: &Key, one: &Key, other: &Key) -> String {
    let (low, high) = if one <= other {
        (one, other)
    } else {
        (other, one)
    };
    let mut out = [0u8; 4];
    expand(
        shared,
        &[b"hearth-code".as_slice(), low, high].concat(),
        &mut out,
    );
    format!("{:06}", u32::from_be_bytes(out) % 1_000_000)
}

/// Compares a typed code, ignoring spaces, in constant time.
pub fn codes_match(code: &str, typed: &str) -> bool {
    let typed: String = typed.chars().filter(char::is_ascii_digit).collect();
    code.len() == typed.len() && bool::from(code.as_bytes().ct_eq(typed.as_bytes()))
}

pub fn derive(secret: &Key, purpose: &[u8]) -> Key {
    let mut out = [0u8; 32];
    expand(secret, purpose, &mut out);
    out
}

fn expand(secret: &[u8], info: &[u8], out: &mut [u8]) {
    Hkdf::<Sha256>::new(None, secret)
        .expand(info, out)
        .expect("a short output");
}

/// Encrypts and authenticates `plain`, bound to `context`.
pub fn seal(key: &Key, context: &[u8], plain: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let sealed = ChaCha20Poly1305::new(key.into())
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plain,
                aad: context,
            },
        )
        .expect("a message within ChaCha20's limit");
    [nonce.as_slice(), &sealed].concat()
}

pub fn open(key: &Key, context: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() < 12 {
        anyhow::bail!("a message too short to open");
    }
    let (nonce, body) = sealed.split_at(12);
    ChaCha20Poly1305::new(key.into())
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: body,
                aad: context,
            },
        )
        .map_err(|_| anyhow!("a message that failed its check"))
}

pub fn tag(key: &Key, data: &[u8]) -> Vec<u8> {
    hmac(key, data).finalize().into_bytes().to_vec()
}

pub fn tag_ok(key: &Key, data: &[u8], tag: &[u8]) -> bool {
    hmac(key, data).verify_slice(tag).is_ok()
}

fn hmac(key: &Key, data: &[u8]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
    mac.update(data);
    mac
}

pub fn encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn decode(text: &str) -> Result<Vec<u8>> {
    STANDARD.decode(text).context("bad base64")
}

pub fn decode_key(text: &str) -> Result<Key> {
    decode(text)?
        .try_into()
        .map_err(|_| anyhow!("a key of the wrong size"))
}

/// What every member of a group holds, in `group.json`.
#[derive(Clone, Serialize, Deserialize)]
pub struct GroupKey {
    pub id: String,
    #[serde(with = "base64_key")]
    secret: Key,
}

impl GroupKey {
    pub fn new() -> Self {
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        Self {
            id: format!("{:016x}", rand::random::<u64>()),
            secret,
        }
    }

    /// Signs hellos.
    pub fn hello(&self) -> Key {
        derive(&self.secret, b"hearth-hello")
    }

    /// Seals requests and replies.
    pub fn message(&self) -> Key {
        derive(&self.secret, b"hearth-message")
    }

    pub fn load() -> Option<Self> {
        serde_json::from_str(&fs::read_to_string(path().ok()?).ok()?).ok()
    }

    pub fn save(&self) -> Result<()> {
        crate::files::write(&path()?, &serde_json::to_vec(self)?)
    }

    pub fn delete() -> Result<()> {
        match fs::remove_file(path()?) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }
}

fn path() -> Result<PathBuf> {
    Ok(crate::config::config_dir()?.join("group.json"))
}

mod base64_key {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(key: &super::Key, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::encode(key))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<super::Key, D::Error> {
        super::decode_key(&String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_show_the_same_code_unless_someone_is_between() {
        let (one, other, between) = (Ephemeral::new(), Ephemeral::new(), Ephemeral::new());
        let (one_public, other_public, between_public) = (one.public, other.public, between.public);
        let shared = one.agree(&other_public);
        assert_eq!(shared, other.agree(&one_public));
        assert_eq!(
            code(&shared, &one_public, &other_public),
            code(&shared, &other_public, &one_public)
        );
        let intercepted = Ephemeral::new().agree(&between_public);
        assert_ne!(
            code(&shared, &one_public, &other_public),
            code(&intercepted, &one_public, &between_public)
        );
        assert!(codes_match("482193", "482 193"));
        assert!(!codes_match("482193", "482194"));
    }

    #[test]
    fn a_sealed_message_opens_only_unchanged_and_in_its_context() {
        let key = GroupKey::new().message();
        let sealed = seal(&key, b"nonce", b"secret");
        assert_eq!(open(&key, b"nonce", &sealed).unwrap(), b"secret");
        assert!(open(&key, b"other nonce", &sealed).is_err());
        let mut changed = sealed.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(open(&key, b"nonce", &changed).is_err());

        let hello = GroupKey::new().hello();
        assert!(tag_ok(&hello, b"serving", &tag(&hello, b"serving")));
        assert!(!tag_ok(&hello, b"standby", &tag(&hello, b"serving")));
    }
}
