//! The group key, and the handshake that shares it.
//!
//! A computer with the key approves a new one by the code on its screen.
//! X25519 agrees a secret, both sides derive the same code, and the key is
//! sealed with ChaCha20-Poly1305. A different code means someone is in between.

use anyhow::{anyhow, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use x25519_dalek::{EphemeralSecret, PublicKey};

const KEY_LEN: usize = 32;

pub struct Ephemeral {
    secret: EphemeralSecret,
    pub public: [u8; KEY_LEN],
}

impl Ephemeral {
    pub fn generate() -> Self {
        let secret = EphemeralSecret::random_from_rng(rand::thread_rng());
        let public = PublicKey::from(&secret).to_bytes();
        Self { secret, public }
    }

    pub fn agree(self, peer: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
        let shared = self.secret.diffie_hellman(&PublicKey::from(*peer));
        *shared.as_bytes()
    }
}

pub fn parse_public(text: &str) -> Result<[u8; KEY_LEN]> {
    let bytes = decode_hex(text)?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("the handshake key is the wrong size"))
}

/// Six digits, the same on both screens only when nothing is in between.
pub fn sas(shared: &[u8], left: &[u8; KEY_LEN], right: &[u8; KEY_LEN]) -> String {
    let (first, second) = if left <= right {
        (left, right)
    } else {
        (right, left)
    };
    let mut info = b"hearth-sas-v1".to_vec();
    info.extend_from_slice(first);
    info.extend_from_slice(second);
    let mut out = [0u8; 4];
    expand(shared, &info, &mut out);
    let number = u32::from_be_bytes(out) % 1_000_000;
    format!("{number:06}")
}

pub fn format_code(digits: &str) -> String {
    if digits.len() == 6 {
        format!("{} {}", &digits[..3], &digits[3..])
    } else {
        digits.to_owned()
    }
}

pub fn codes_match(expected: &str, typed: &str) -> bool {
    let typed: String = typed.chars().filter(|char| char.is_ascii_digit()).collect();
    if expected.len() != typed.len() {
        return false;
    }
    expected.as_bytes().ct_eq(typed.as_bytes()).into()
}

pub fn derive(shared: &[u8], info: &[u8]) -> [u8; KEY_LEN] {
    let mut out = [0u8; KEY_LEN];
    expand(shared, info, &mut out);
    out
}

fn expand(secret: &[u8], info: &[u8], out: &mut [u8]) {
    Hkdf::<Sha256>::new(None, secret)
        .expand(info, out)
        .expect("output length is within HKDF's limit");
}

pub fn seal(key: &[u8; KEY_LEN], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).context("bad key")?;
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let cipher_text = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), Payload { msg: plain, aad })
        .map_err(|_| anyhow!("could not seal"))?;
    let mut out = nonce_bytes.to_vec();
    out.extend(cipher_text);
    Ok(out)
}

pub fn open(key: &[u8; KEY_LEN], aad: &[u8], packed: &[u8]) -> Result<Vec<u8>> {
    if packed.len() < 12 + 16 {
        anyhow::bail!("truncated message");
    }
    let (nonce, cipher_text) = packed.split_at(12);
    let cipher = ChaCha20Poly1305::new_from_slice(key).context("bad key")?;
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: cipher_text,
                aad,
            },
        )
        .map_err(|_| anyhow!("the message failed its check"))
}

pub fn mac(key: &[u8; KEY_LEN], body: &[u8]) -> [u8; KEY_LEN] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("key length");
    mac.update(body);
    mac.finalize().into_bytes().into()
}

pub fn mac_ok(key: &[u8; KEY_LEN], body: &[u8], tag: &[u8]) -> bool {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("key length");
    mac.update(body);
    mac.verify_slice(tag).is_ok()
}

#[derive(Clone)]
pub struct ClusterKey {
    pub id: String,
    key: [u8; KEY_LEN],
}

impl ClusterKey {
    pub fn generate() -> Self {
        let mut key = [0u8; KEY_LEN];
        rand::thread_rng().fill_bytes(&mut key);
        Self {
            id: format!("{:016x}", rand::random::<u64>()),
            key,
        }
    }

    pub fn hello_key(&self) -> [u8; KEY_LEN] {
        derive(&self.key, b"hearth-hello-v1")
    }

    pub fn state_key(&self) -> [u8; KEY_LEN] {
        derive(&self.key, b"hearth-state-v1")
    }

    pub fn encode_key(&self) -> String {
        encode_hex(&self.key)
    }

    pub fn from_parts(id: &str, key_hex: &str) -> Result<Self> {
        if id.is_empty() {
            anyhow::bail!("the group has no id");
        }
        let key = decode_hex(key_hex)?
            .try_into()
            .map_err(|_| anyhow!("the group key is the wrong size"))?;
        Ok(Self {
            id: id.to_owned(),
            key,
        })
    }

    pub fn load() -> Result<Self> {
        let text = std::fs::read_to_string(path()?).context("no group yet")?;
        let stored: Stored = serde_json::from_str(&text).context("the group file is unreadable")?;
        Self::from_parts(&stored.id, &stored.key)
    }

    pub fn save(&self) -> Result<()> {
        let dir = crate::config::config_dir()?;
        std::fs::create_dir_all(&dir)?;
        let text = serde_json::to_string(&Stored {
            id: self.id.clone(),
            key: self.encode_key(),
        })?;
        let target = path()?;
        let tmp = dir.join("cluster.json.tmp");
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(text.as_bytes())?;
        }
        std::fs::rename(tmp, target)?;
        Ok(())
    }

    pub fn delete() -> Result<()> {
        match path() {
            Ok(path) => {
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    id: String,
    key: String,
}

fn path() -> Result<std::path::PathBuf> {
    Ok(crate::config::config_dir()?.join("cluster.json"))
}

pub fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn decode_hex(text: &str) -> Result<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        anyhow::bail!("bad hex");
    }
    (0..text.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&text[index..index + 2], 16).map_err(|_| anyhow!("bad hex"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_see_the_same_code_unless_someone_is_between() {
        let alice = Ephemeral::generate();
        let bob = Ephemeral::generate();
        let eve = Ephemeral::generate();
        let alice_pub = alice.public;
        let bob_pub = bob.public;
        let shared_ab = alice.agree(&bob_pub);
        let shared_ba = bob.agree(&alice_pub);
        assert_eq!(shared_ab, shared_ba);
        assert_eq!(
            sas(&shared_ab, &alice_pub, &bob_pub),
            sas(&shared_ba, &bob_pub, &alice_pub)
        );
        let shared_ae = Ephemeral::generate().agree(&eve.public);
        assert_ne!(
            sas(&shared_ab, &alice_pub, &bob_pub),
            sas(&shared_ae, &alice_pub, &eve.public)
        );
    }

    #[test]
    fn a_changed_byte_does_not_open() {
        let key = ClusterKey::generate();
        let sealed = seal(&key.state_key(), b"state", b"secret").unwrap();
        assert_eq!(
            open(&key.state_key(), b"state", &sealed).unwrap(),
            b"secret"
        );
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(open(&key.state_key(), b"state", &tampered).is_err());
        assert!(open(&key.state_key(), b"ask", &sealed).is_err());
    }

    #[test]
    fn the_announcement_check_sees_a_flipped_bit() {
        let key = ClusterKey::generate().hello_key();
        let body = b"leader";
        let tag = mac(&key, body);
        assert!(mac_ok(&key, body, &tag));
        assert!(!mac_ok(&key, b"follower", &tag));
    }

    #[test]
    fn the_typed_code_ignores_spaces() {
        assert!(codes_match("482193", "482 193"));
        assert!(!codes_match("482193", "482194"));
    }
}
