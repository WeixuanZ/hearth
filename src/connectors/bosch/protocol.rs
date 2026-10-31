//! The local Home Connect messages, and the AES seal older appliances use.
//!
//! A modern dishwasher (the SPV2HKX42G among them) speaks JSON in the clear
//! inside TLS-PSK. An older machine uses HTTP and seals each frame itself:
//! AES-256-CBC, with an HMAC that chains so a captured frame cannot be
//! replayed. The key material comes from the one-time account sign-in.

use aes::Aes256;
use anyhow::{anyhow, Result};
use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::sign::Signer;
use rand::RngCore;
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::config::BoschFeature;

pub struct Seal {
    mac_key: [u8; 32],
    /// The IV mixed into every tag. It does not advance.
    iv: [u8; 16],
    enc_iv: [u8; 16],
    dec_iv: [u8; 16],
    last_tx: [u8; 16],
    last_rx: [u8; 16],
    cipher: Aes256,
    /// Tag byte on frames we send. A client uses `E` (0x45); the machine checks that.
    send: u8,
    /// Tag byte on frames we accept. A client checks `C` (0x43), which is how the machine signs.
    recv: u8,
}

impl Seal {
    pub fn new(key_b64: &str, iv_b64: &str) -> Result<Self> {
        Self::facing(key_b64, iv_b64, 0x45, 0x43)
    }

    /// The machine's end of the same key. It signs what [`Seal::open`] checks.
    #[cfg(test)]
    fn peer(key_b64: &str, iv_b64: &str) -> Result<Self> {
        Self::facing(key_b64, iv_b64, 0x43, 0x45)
    }

    fn facing(key_b64: &str, iv_b64: &str, send: u8, recv: u8) -> Result<Self> {
        let raw = decode_key(key_b64)?;
        let key = hmac_sha256(&raw, b"ENC");
        let mac_key = hmac_sha256(&raw, b"MAC");
        let iv_bytes = decode_key(iv_b64)?;
        let iv: [u8; 16] = iv_bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("the appliance iv is the wrong size"))?;
        Ok(Self {
            cipher: Aes256::new_from_slice(&key).expect("aes-256 key is 32 bytes"),
            mac_key,
            iv,
            enc_iv: iv,
            dec_iv: iv,
            last_tx: [0; 16],
            last_rx: [0; 16],
            send,
            recv,
        })
    }

    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        let mut body = pad(plain);
        cbc(true, &self.cipher, &mut self.enc_iv, &mut body);
        let tag = tag(&self.mac_key, &self.iv, self.send, &self.last_tx, &body);
        self.last_tx = tag;
        body.extend(tag);
        body
    }

    pub fn open(&mut self, packed: &[u8]) -> Result<Vec<u8>> {
        if packed.len() < 32 || !packed.len().is_multiple_of(16) {
            anyhow::bail!("truncated appliance message");
        }
        let (body, tag_bytes) = packed.split_at(packed.len() - 16);
        let got: [u8; 16] = tag_bytes.try_into().expect("16");
        let expect = tag(&self.mac_key, &self.iv, self.recv, &self.last_rx, body);
        if expect != got {
            anyhow::bail!("the appliance message failed its check");
        }
        self.last_rx = got;
        let mut plain = body.to_vec();
        cbc(false, &self.cipher, &mut self.dec_iv, &mut plain);
        let pad_len = *plain.last().unwrap_or(&0) as usize;
        if pad_len == 0 || pad_len > plain.len() {
            anyhow::bail!("the appliance message had bad padding");
        }
        plain.truncate(plain.len() - pad_len);
        Ok(plain)
    }
}

fn pad(msg: &[u8]) -> Vec<u8> {
    let mut pad_len = 16 - (msg.len() % 16);
    if pad_len == 1 {
        pad_len += 16;
    }
    let mut out = msg.to_vec();
    out.push(0);
    let mut random = vec![0u8; pad_len - 2];
    rand::thread_rng().fill_bytes(&mut random);
    out.extend(random);
    out.push(u8::try_from(pad_len).expect("pad fits a byte"));
    out
}

fn tag(mac_key: &[u8; 32], iv: &[u8; 16], direction: u8, prev: &[u8; 16], body: &[u8]) -> [u8; 16] {
    let mut msg = Vec::with_capacity(iv.len() + 1 + prev.len() + body.len());
    msg.extend_from_slice(iv);
    msg.push(direction);
    msg.extend_from_slice(prev);
    msg.extend_from_slice(body);
    let full = hmac_sha256(mac_key, &msg);
    full[..16].try_into().expect("16")
}

fn cbc(encrypt: bool, cipher: &Aes256, iv: &mut [u8; 16], buf: &mut [u8]) {
    for chunk in buf.chunks_mut(16) {
        if encrypt {
            for (byte, prev) in chunk.iter_mut().zip(iv.iter()) {
                *byte ^= prev;
            }
            let mut block = cipher::generic_array::GenericArray::clone_from_slice(chunk);
            cipher.encrypt_block(&mut block);
            chunk.copy_from_slice(&block);
            iv.copy_from_slice(chunk);
        } else {
            let next = {
                let mut next = [0u8; 16];
                next.copy_from_slice(chunk);
                next
            };
            let mut block = cipher::generic_array::GenericArray::clone_from_slice(chunk);
            cipher.decrypt_block(&mut block);
            chunk.copy_from_slice(&block);
            for (byte, prev) in chunk.iter_mut().zip(iv.iter()) {
                *byte ^= prev;
            }
            *iv = next;
        }
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let pkey = PKey::hmac(key).expect("hmac key");
    let mut signer = Signer::new(MessageDigest::sha256(), &pkey).expect("signer");
    signer.update(data).expect("hmac data");
    let mut out = [0u8; 32];
    signer.sign(&mut out).expect("hmac out");
    out
}

pub fn decode_key(text: &str) -> Result<Vec<u8>> {
    let mut padded = text.trim().to_owned();
    while !padded.len().is_multiple_of(4) {
        padded.push('=');
    }
    URL_SAFE
        .decode(padded)
        .map_err(|_| anyhow!("the appliance key is not valid"))
}

/// What Home, and the menu, need from the live machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub power: bool,
    pub operation: String,
    pub program: String,
    pub door_open: bool,
    pub finished: bool,
    pub leak: bool,
    pub salt_low: Option<bool>,
    pub rinse_low: Option<bool>,
    pub remote_start: bool,
    pub remaining_secs: u64,
    pub progress: u64,
    pub energy: String,
    pub water: String,
    pub options: BTreeMap<String, bool>,
}

impl Machine {
    /// Running a programme, or about to: the only time its remaining time
    /// and progress mean anything. Otherwise they are the last programme's.
    pub fn running(&self) -> bool {
        self.operation.ends_with("Run") || self.operation.ends_with("DelayedStart")
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self {
            power: false,
            operation: "Inactive".into(),
            program: String::new(),
            door_open: false,
            finished: false,
            leak: false,
            salt_low: None,
            rinse_low: None,
            remote_start: false,
            remaining_secs: 0,
            progress: 0,
            energy: String::new(),
            water: String::new(),
            options: BTreeMap::new(),
        }
    }
}

/// A reading the bridge is currently showing. `online` is the socket, not the cycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reading {
    pub online: bool,
    pub machine: Machine,
}

pub fn apply_value(machine: &mut Machine, name: &str, value: &str) {
    if name.ends_with("PowerState") {
        let value = value.to_ascii_lowercase();
        machine.power = matches!(value.as_str(), "on" | "true" | "1");
    } else if name.ends_with("DoorState") {
        let value = value.to_ascii_lowercase();
        machine.door_open = value.contains("open") || value == "ajar";
    } else if name.ends_with("OperationState") {
        machine.operation = value.to_owned();
    } else if name.ends_with("ProgramFinished") {
        machine.finished = flagged(value);
    } else if name.contains("AquaStop") {
        machine.leak = flagged(value);
    } else if salt_name(name) {
        machine.salt_low = Some(flagged(value));
    } else if name.contains("RinseAid") {
        machine.rinse_low = Some(flagged(value));
    } else if name.ends_with("RemoteControlStartAllowed") {
        machine.remote_start = flagged(value);
    } else if name.ends_with("RemainingProgramTime") && !name.contains("Estimated") {
        machine.remaining_secs = value.parse().unwrap_or(0);
    } else if name.ends_with("ProgramProgress") {
        machine.progress = value.parse().unwrap_or(0);
    } else if name.ends_with("EnergyForecast") {
        machine.energy = value.to_owned();
    } else if name.ends_with("WaterForecast") {
        machine.water = value.to_owned();
    } else if name.ends_with("ActiveProgram") || name.ends_with("SelectedProgram") {
        if value != "0" && !value.is_empty() && !value.eq_ignore_ascii_case("none") {
            machine.program = value.to_owned();
        }
    } else if let Some(key) = option_key(name) {
        machine.options.insert(key.to_owned(), flagged(value));
    }
}

fn flagged(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "on" | "present" | "1" | "low" | "empty" | "locked"
    )
}

pub(crate) fn salt_name(name: &str) -> bool {
    name.contains("Salt") && !name.contains("Program") && !name.contains("Option")
}

/// Option switches, in the order their HomeKit iids are fixed.
const OPTIONS: &[(&str, &str)] = &[
    ("ExtraDry", "Extra Dry"),
    ("VarioSpeedPlus", "SpeedPerfect"),
    ("SilenceOnDemand", "Silence"),
    ("HygienePlus", "Hygiene"),
    ("IntensiveZone", "Intensive zone"),
    ("ChildLock", "Child lock"),
    ("PermanentRemoteStart", "Remote start"),
];

pub fn option_iid(key: &str) -> Option<u64> {
    OPTIONS
        .iter()
        .position(|(name, _)| *name == key)
        .map(|index| 300 + u64::try_from(index).expect("a handful of options") * 10)
}

fn option_key(name: &str) -> Option<&'static str> {
    if name.ends_with("VarioSpeedPlus") || name.ends_with("VarioSpeed") {
        return Some("VarioSpeedPlus");
    }
    if name.ends_with("SilenceOnDemand") || name.ends_with("Silence") {
        return Some("SilenceOnDemand");
    }
    if name.ends_with("HygienePlus") || name.ends_with("Hygiene") {
        return Some("HygienePlus");
    }
    OPTIONS
        .iter()
        .find(|(key, _)| name.ends_with(key))
        .map(|(key, _)| *key)
}

/// Options this machine actually has, with the iid Home will keep.
pub fn option_switches(
    features: &BTreeMap<String, BoschFeature>,
) -> Vec<(&'static str, u64, &'static str)> {
    OPTIONS
        .iter()
        .filter_map(|(key, label)| {
            let iid = option_iid(key)?;
            feature_uid(features, key).map(|_| (*key, iid, *label))
        })
        .collect()
}

/// Programmes the machine itself lists, in uid order so Home's tiles stay put.
pub fn programs(features: &BTreeMap<String, BoschFeature>) -> Vec<(u32, String)> {
    let mut found: Vec<(u32, String)> = features
        .iter()
        .filter_map(|(uid, feature)| {
            if !feature.name.contains(".Program.") {
                return None;
            }
            let uid = uid.parse().ok()?;
            Some((uid, feature.name.clone()))
        })
        .collect();
    found.sort_by_key(|(uid, _)| *uid);
    found.truncate(16);
    found
}

pub fn feature_uid(features: &BTreeMap<String, BoschFeature>, suffix: &str) -> Option<u32> {
    let aliases: &[&str] = match suffix {
        "VarioSpeedPlus" => &["VarioSpeedPlus", "VarioSpeed"],
        "SilenceOnDemand" => &["SilenceOnDemand", "Silence"],
        "HygienePlus" => &["HygienePlus", "Hygiene"],
        other => return feature_ending(features, other),
    };
    aliases
        .iter()
        .find_map(|alias| feature_ending(features, alias))
}

fn feature_ending(features: &BTreeMap<String, BoschFeature>, suffix: &str) -> Option<u32> {
    features.iter().find_map(|(uid, feature)| {
        feature
            .name
            .ends_with(suffix)
            .then(|| uid.parse().ok())
            .flatten()
    })
}

pub fn enum_value(feature: &BoschFeature, name: &str) -> Option<Value> {
    feature.values.iter().find_map(|(key, label)| {
        if label.eq_ignore_ascii_case(name) {
            key.parse::<u64>().ok().map(|number| json!(number))
        } else {
            None
        }
    })
}

pub fn describe(
    features: &BTreeMap<String, BoschFeature>,
    uid: &str,
    value: &Value,
) -> Option<(String, String)> {
    let feature = features.get(uid)?;
    let text = match value {
        Value::Number(number) => feature
            .values
            .get(&number.to_string())
            .cloned()
            .unwrap_or_else(|| number.to_string()),
        Value::Bool(on) => {
            if *on {
                "true".into()
            } else {
                "false".into()
            }
        }
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    Some((feature.name.clone(), text))
}

pub fn label_of(name: &str) -> String {
    let short = name.rsplit('.').next().unwrap_or(name);
    let mut out = String::new();
    let chars: Vec<char> = short.chars().collect();
    for (index, ch) in chars.iter().copied().enumerate() {
        if index > 0 {
            let prev = chars[index - 1];
            if ch.is_ascii_uppercase() || (ch.is_ascii_digit() && prev.is_ascii_alphabetic()) {
                out.push(' ');
            }
        }
        out.push(ch);
    }
    out
}

pub fn format_remaining(seconds: u64) -> String {
    if seconds == 0 {
        return "Done".into();
    }
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    if hours == 0 {
        format!("{minutes} min")
    } else {
        format!("{hours} h {minutes} min")
    }
}

/// What Home asked for. Queued until the machine has finished its handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Power(bool),
    Option { key: String, on: bool },
    Program(u32),
}

/// One Home Connect session: ids, the service list, and the replies we owe.
#[derive(Debug)]
pub struct Talk {
    sid: i64,
    next_id: i64,
    services: BTreeMap<String, i64>,
    ready: bool,
    synced: bool,
    queued: Vec<Command>,
}

impl Talk {
    pub fn new() -> Self {
        Self {
            sid: 0,
            next_id: 0,
            services: BTreeMap::new(),
            ready: false,
            synced: false,
            queued: Vec::new(),
        }
    }

    pub fn synced(&self) -> bool {
        self.synced
    }

    pub fn started(&self) -> bool {
        self.sid != 0 || self.ready
    }

    pub fn ready(&self) -> bool {
        self.ready
    }

    /// Asks for a full reading. Also keeps a quiet socket from looking dead.
    pub fn poll(&mut self) -> Option<String> {
        if !self.ready {
            return None;
        }
        Some(self.get("/ro/allMandatoryValues", None))
    }

    /// Replies to send, in order. A command sent before the handshake waits.
    pub fn submit(
        &mut self,
        features: &BTreeMap<String, BoschFeature>,
        command: Command,
    ) -> Option<String> {
        if !self.ready {
            self.queued.push(command);
            return None;
        }
        self.encode(features, command)
    }

    pub fn intake(
        &mut self,
        features: &BTreeMap<String, BoschFeature>,
        machine: &mut Machine,
        text: &str,
    ) -> Vec<String> {
        let Ok(msg) = serde_json::from_str::<Value>(text) else {
            return Vec::new();
        };
        let resource = text_at(&msg, "resource");
        let action = text_at(&msg, "action");
        if let Some(code) = msg.get("code").filter(|code| !code.is_null()) {
            log::warn!("dishwasher {resource} returned {code}");
        }
        if action == "POST" && resource == "/ei/initialValues" {
            return self.hello(&msg);
        }
        let mut out = Vec::new();
        if resource == "/ci/services" && !self.ready {
            self.remember_services(&msg);
            self.ready = true;
            out.extend(self.introduce());
            out.extend(self.flush(features));
        }
        if matches!(
            resource,
            "/ro/values"
                | "/ro/allMandatoryValues"
                | "/ro/allDescriptionChanges"
                | "/ro/descriptionChange"
        ) {
            if let Some(data) = msg.get("data") {
                absorb(features, machine, data);
            }
            if resource == "/ro/allMandatoryValues" {
                self.synced = true;
            }
        }
        if matches!(resource, "/ro/activeProgram" | "/ro/selectedProgram") {
            if let Some(program) = msg.get("data").and_then(|data| data.get(0)?.get("program")) {
                note_program(features, machine, program);
            }
        }
        out
    }

    fn hello(&mut self, msg: &Value) -> Vec<String> {
        self.sid = number_at(msg, "sID").unwrap_or(0);
        self.next_id = msg
            .get("data")
            .and_then(|data| data.get(0))
            .and_then(|data| number_at(data, "edMsgID"))
            .unwrap_or(1);
        let version = number_at(msg, "version").unwrap_or(1);
        let reply_id = number_at(msg, "msgID").unwrap_or(0);
        let device_type = if version == 1 {
            json!(2)
        } else {
            json!("Application")
        };
        let reply = self.frame(
            reply_id,
            "/ei/initialValues",
            version,
            "RESPONSE",
            Some(json!([{
                "deviceType": device_type,
                "deviceName": "hearth",
                "deviceID": "hearth",
            }])),
        );
        vec![reply, self.get("/ci/services", None)]
    }

    fn remember_services(&mut self, msg: &Value) {
        let Some(items) = msg.get("data").and_then(Value::as_array) else {
            return;
        };
        for item in items {
            let Some(name) = item
                .get("service")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let version = number_at(item, "version").unwrap_or(1);
            self.services.insert(name.to_owned(), version);
        }
    }

    fn introduce(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if self.services.contains_key("iz") {
            out.push(self.get("/iz/info", None));
        } else {
            let mut bytes = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut bytes);
            out.push(self.get(
                "/ci/authentication",
                Some(json!([{ "nonce": URL_SAFE_NO_PAD.encode(bytes) }])),
            ));
            out.push(self.get("/ci/info", None));
        }
        out.push(self.note("/ei/deviceReady"));
        if self.services.contains_key("ni") {
            out.push(self.get("/ni/info", None));
        }
        out.push(self.get("/ro/allMandatoryValues", None));
        out.push(self.get("/ro/allDescriptionChanges", None));
        out
    }

    fn flush(&mut self, features: &BTreeMap<String, BoschFeature>) -> Vec<String> {
        std::mem::take(&mut self.queued)
            .into_iter()
            .filter_map(|command| self.encode(features, command))
            .collect()
    }

    fn encode(
        &mut self,
        features: &BTreeMap<String, BoschFeature>,
        command: Command,
    ) -> Option<String> {
        let (resource, data) = match command {
            Command::Power(on) => {
                let uid = feature_uid(features, "PowerState")?;
                let feature = features.get(&uid.to_string())?;
                (
                    "/ro/values",
                    json!([{ "uid": uid, "value": switched(feature, on) }]),
                )
            }
            Command::Option { key, on } => {
                let uid = feature_uid(features, &key)?;
                let feature = features.get(&uid.to_string())?;
                (
                    "/ro/values",
                    json!([{ "uid": uid, "value": switched(feature, on) }]),
                )
            }
            Command::Program(uid) => (
                "/ro/activeProgram",
                json!([{ "program": uid, "options": [] }]),
            ),
        };
        Some(self.post(resource, data))
    }

    fn get(&mut self, resource: &str, data: Option<Value>) -> String {
        let version = self.version_of(resource, 1);
        self.send(resource, version, "GET", data)
    }

    fn note(&mut self, resource: &str) -> String {
        let version = self.version_of(resource, 2);
        self.send(resource, version, "NOTIFY", None)
    }

    fn post(&mut self, resource: &str, data: Value) -> String {
        let version = self.version_of(resource, 1);
        self.send(resource, version, "POST", Some(data))
    }

    fn send(&mut self, resource: &str, version: i64, action: &str, data: Option<Value>) -> String {
        let id = self.next_id;
        self.next_id += 1;
        self.frame(id, resource, version, action, data)
    }

    fn frame(
        &self,
        msg_id: i64,
        resource: &str,
        version: i64,
        action: &str,
        data: Option<Value>,
    ) -> String {
        let mut msg = json!({
            "sID": self.sid,
            "msgID": msg_id,
            "resource": resource,
            "version": version,
            "action": action,
        });
        if let Some(data) = data {
            msg["data"] = data;
        }
        serde_json::to_string(&msg).expect("a json object serialises")
    }

    fn version_of(&self, resource: &str, default: i64) -> i64 {
        let service = resource.split('/').nth(1).unwrap_or("");
        self.services.get(service).copied().unwrap_or(default)
    }
}

fn switched(feature: &BoschFeature, on: bool) -> Value {
    let names: &[&str] = if on {
        &["On", "True", "true"]
    } else {
        &["Off", "False", "false"]
    };
    for name in names {
        if let Some(value) = enum_value(feature, name) {
            return value;
        }
    }
    json!(i64::from(on))
}

fn absorb(features: &BTreeMap<String, BoschFeature>, machine: &mut Machine, data: &Value) {
    let Some(items) = data.as_array() else {
        return;
    };
    for item in items {
        let Some(uid) = item.get("uid").and_then(json_key) else {
            continue;
        };
        let Some(value) = item.get("value") else {
            continue;
        };
        let Some((name, text)) = describe(features, &uid, value) else {
            continue;
        };
        // The machine names a programme by its uid: its name is in the map.
        if name.ends_with("ActiveProgram") || name.ends_with("SelectedProgram") {
            note_program(features, machine, value);
        } else {
            apply_value(machine, &name, &text);
        }
    }
}

fn note_program(features: &BTreeMap<String, BoschFeature>, machine: &mut Machine, value: &Value) {
    let Some(uid) = json_key(value) else {
        return;
    };
    let text = features
        .get(&uid)
        .map(|feature| feature.name.clone())
        .unwrap_or(uid);
    apply_value(machine, "BSH.Common.Root.ActiveProgram", &text);
}

fn json_key(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => Some(number.to_string()),
        Value::String(text) => Some(text.clone()),
        _ => None,
    }
}

fn text_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn number_at(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_frame_roundtrips_and_rejects_a_flipped_byte() {
        let mut machine = Seal::peer(
            "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA",
            "AQIDBAUGBwgJCgsMDQ4PEA",
        )
        .unwrap();
        let mut hearth = Seal::new(
            "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA",
            "AQIDBAUGBwgJCgsMDQ4PEA",
        )
        .unwrap();
        let packed = machine.seal(br#"{"resource":"/ro/values"}"#);
        let plain = hearth.open(&packed).unwrap();
        assert_eq!(plain, br#"{"resource":"/ro/values"}"#);
        let outbound = hearth.seal(br#"{"action":"POST"}"#);
        assert_eq!(machine.open(&outbound).unwrap(), br#"{"action":"POST"}"#);
        let mut bad = packed.clone();
        bad[0] ^= 0x01;
        assert!(hearth.open(&bad).is_err());
    }

    #[test]
    fn two_frames_stay_in_order() {
        let mut tx = Seal::peer(
            "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA",
            "AQIDBAUGBwgJCgsMDQ4PEA",
        )
        .unwrap();
        let mut rx = Seal::new(
            "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA",
            "AQIDBAUGBwgJCgsMDQ4PEA",
        )
        .unwrap();
        let first = tx.seal(b"one");
        let second = tx.seal(b"two");
        assert_eq!(rx.open(&first).unwrap(), b"one");
        assert_eq!(rx.open(&second).unwrap(), b"two");
        assert!(rx.open(&first).is_err());
    }

    #[test]
    fn readings_become_the_fields_home_shows() {
        let mut machine = Machine::default();
        apply_value(&mut machine, "BSH.Common.Setting.PowerState", "On");
        apply_value(&mut machine, "BSH.Common.Status.DoorState", "Open");
        apply_value(&mut machine, "BSH.Common.Status.DoorState", "Locked");
        apply_value(&mut machine, "BSH.Common.Event.ProgramFinished", "Present");
        apply_value(&mut machine, "BSH.Common.Event.AquaStopOccured", "Off");
        apply_value(
            &mut machine,
            "Dishcare.Dishwasher.Event.SaltNearlyEmpty",
            "true",
        );
        apply_value(&mut machine, "Dishcare.Dishwasher.Option.ExtraDry", "On");
        apply_value(
            &mut machine,
            "BSH.Common.Option.RemainingProgramTime",
            "5400",
        );
        apply_value(&mut machine, "BSH.Common.Root.ActiveProgram", "Eco50");
        assert!(machine.power);
        assert!(!machine.door_open);
        assert!(machine.finished);
        assert!(!machine.leak);
        assert!(machine.salt_low == Some(true));
        assert_eq!(machine.options.get("ExtraDry"), Some(&true));
        assert_eq!(machine.remaining_secs, 5400);
        assert_eq!(machine.program, "Eco50");
        assert_eq!(format_remaining(5400), "1 h 30 min");
    }

    #[test]
    fn programme_names_follow_the_uid_so_tiles_do_not_move() {
        let mut features = BTreeMap::new();
        features.insert(
            "30".into(),
            BoschFeature {
                name: "Dishcare.Dishwasher.Program.Eco50".into(),
                values: BTreeMap::new(),
            },
        );
        features.insert(
            "10".into(),
            BoschFeature {
                name: "Dishcare.Dishwasher.Program.Intensiv70".into(),
                values: BTreeMap::new(),
            },
        );
        features.insert(
            "11".into(),
            BoschFeature {
                name: "BSH.Common.Setting.PowerState".into(),
                values: BTreeMap::new(),
            },
        );
        let names: Vec<_> = programs(&features)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(
            names,
            vec![
                "Dishcare.Dishwasher.Program.Intensiv70".to_owned(),
                "Dishcare.Dishwasher.Program.Eco50".to_owned(),
            ]
        );
        assert_eq!(label_of(&names[0]), "Intensiv 70");
    }

    #[test]
    fn the_handshake_replies_then_sends_a_queued_power_command() {
        let mut features = BTreeMap::new();
        let mut values = BTreeMap::new();
        values.insert("0".into(), "Off".into());
        values.insert("1".into(), "On".into());
        features.insert(
            "512".into(),
            BoschFeature {
                name: "BSH.Common.Setting.PowerState".into(),
                values,
            },
        );
        let mut talk = Talk::new();
        let mut machine = Machine::default();
        let hello = talk.intake(
            &features,
            &mut machine,
            r#"{"sID":7,"msgID":3,"resource":"/ei/initialValues","version":2,"action":"POST","data":[{"edMsgID":40}]}"#,
        );
        assert!(hello[0].starts_with(r#"{"sID":7,"#));
        assert!(hello[0].contains(r#""deviceType":"Application""#));
        assert!(hello[1].contains(r#""resource":"/ci/services""#));
        assert!(hello[1].contains(r#""msgID":40"#));
        assert!(talk.submit(&features, Command::Power(true)).is_none());
        let replies = talk.intake(
            &features,
            &mut machine,
            r#"{"resource":"/ci/services","action":"RESPONSE","data":[{"service":"ci","version":2},{"service":"ei","version":2},{"service":"ro","version":1}]}"#,
        );
        assert!(replies.iter().any(|line| line.contains(r#""uid":512"#)));
        talk.intake(
            &features,
            &mut machine,
            r#"{"resource":"/ro/values","action":"NOTIFY","data":[{"uid":512,"value":1}]}"#,
        );
        assert!(machine.power);
        let program = talk.submit(&features, Command::Program(30)).unwrap();
        assert!(program.contains(r#""program":30"#));
        assert!(program.find(r#""program""#).unwrap() < program.find(r#""options""#).unwrap());
    }

    #[test]
    fn a_programme_given_by_its_uid_is_named() {
        let feature = |name: &str| BoschFeature {
            name: name.into(),
            values: BTreeMap::new(),
        };
        let features = BTreeMap::from([
            ("256".to_owned(), feature("BSH.Common.Root.SelectedProgram")),
            ("8196".to_owned(), feature("Dishcare.Dishwasher.Program.Eco50")),
        ]);
        let mut machine = Machine::default();
        absorb(&features, &mut machine, &json!([{ "uid": 256, "value": 8196 }]));
        assert_eq!(label_of(&machine.program), "Eco 50");
        assert!(!machine.running());
        machine.operation = "BSH.Common.EnumType.OperationState.Run".into();
        assert!(machine.running());
    }
}
