//! Decrypt the local MQTT password Dyson returns from the account API.
//!
//! The cloud stores `localBrokerCredentials` as AES-256-CBC with a fixed key
//! and a zero IV (the same constants the MyDyson app uses). The plaintext is
//! JSON, `{"apPasswordHash":"..."}`, padded out to a block. After this one
//! decrypt, the bridge never needs the account again.

use aes::Aes256;
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
use serde_json::Value;

type Aes256CbcDec = cbc::Decryptor<Aes256>;

const KEY: [u8; 32] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 31, 32,
];

pub fn decrypt_local_credentials(encrypted_b64: &str) -> Result<String> {
    let encrypted = base64::engine::general_purpose::STANDARD
        .decode(encrypted_b64.trim())
        .context("local credentials are not base64")?;
    if encrypted.is_empty() || encrypted.len() % 16 != 0 {
        return Err(anyhow!("encrypted credentials are not a whole AES block"));
    }

    let mut buf = encrypted;
    let plain = Aes256CbcDec::new(&KEY.into(), &[0u8; 16].into())
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|_| anyhow!("AES decrypt failed"))?
        .to_vec();

    let text = String::from_utf8(plain).context("decrypted credentials are not text")?;
    let mut stream = serde_json::Deserializer::from_str(&text).into_iter::<Value>();
    let value = stream
        .next()
        .ok_or_else(|| anyhow!("decrypted credentials are not JSON"))?
        .context("decrypted credentials are not JSON")?;
    value
        .get("apPasswordHash")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("decrypted credentials have no apPasswordHash"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cbc::cipher::{BlockEncryptMut, KeyIvInit};

    type Aes256CbcEnc = cbc::Encryptor<Aes256>;

    #[test]
    fn round_trip_backspace_padding() {
        let json = br#"{"apPasswordHash":"local-secret"}"#;
        let mut padded = json.to_vec();
        while !padded.len().is_multiple_of(16) {
            padded.push(0x08);
        }
        let mut buf = padded.clone();
        let encrypted = Aes256CbcEnc::new(&KEY.into(), &[0u8; 16].into())
            .encrypt_padded_mut::<NoPadding>(&mut buf, padded.len())
            .unwrap()
            .to_vec();
        let b64 = base64::engine::general_purpose::STANDARD.encode(encrypted);
        assert_eq!(decrypt_local_credentials(&b64).unwrap(), "local-secret");
    }
}
