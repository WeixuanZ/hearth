//! Dyson local MQTT state, as spoken by the HP09 (product types 527K / 527E).
//!
//! The fan is the broker. Topics are `{productType}/{serial}/status/current`
//! and `{productType}/{serial}/command`. Values are zero-padded strings.
//! Temperatures on the wire are kelvin × 10.

use serde_json::{Map, Value};

pub const DEFAULT_PRODUCT_TYPES: &[&str] = &["527K", "527E", "527"];

#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    Power(bool),
    Auto(bool),
    /// Manual fan speed, 1 through 10.
    Speed(u8),
    Swing(bool),
    /// Oscillate across this many degrees, around where the fan points.
    Sweep(u16),
    Night(bool),
    /// Front jet (focused) versus the wide, diffused stream.
    Focus(bool),
    /// Keep the sensors alive while the fan itself is off.
    Monitor(bool),
    Heat(bool),
    /// Target in Celsius. Sending this also turns heat on.
    HeatTargetC(f32),
}

impl Intent {
    /// Parses `hearth dyson set` arguments, like `power on` or `speed 4`.
    pub fn parse(what: &str, value: &str) -> Option<Self> {
        let on = match value {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        };
        Some(match what {
            "power" => Self::Power(on?),
            "auto" => Self::Auto(on?),
            "speed" => Self::Speed(value.parse().ok().filter(|n| (1..=10).contains(n))?),
            "swing" => match on {
                Some(on) => Self::Swing(on),
                None => Self::Sweep(value.parse().ok()?),
            },
            "night" => Self::Night(on?),
            "focus" => Self::Focus(on?),
            "monitor" => Self::Monitor(on?),
            "heat" => match on {
                Some(on) => Self::Heat(on),
                None => Self::HeatTargetC(value.parse().ok()?),
            },
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MachineState {
    pub power_on: bool,
    pub auto: bool,
    pub speed: Option<u8>,
    pub oscillating: bool,
    /// Some firmware says `OION`/`OIOF` instead of `ON`/`OFF`.
    pub oscillation_extended: bool,
    /// Sweep limits in degrees (`osal`, `osau`). Equal limits sweep nothing.
    pub oscillation_low: Option<u16>,
    pub oscillation_high: Option<u16>,
    pub night: bool,
    pub focus: bool,
    pub monitoring: bool,
    pub heat_on: bool,
    pub heating_now: bool,
    pub heat_target_c: Option<f32>,
    pub temperature_c: Option<f32>,
    pub humidity: Option<f32>,
    pub pm25: Option<f32>,
    pub pm10: Option<f32>,
    pub voc_index: Option<f32>,
    pub no2_index: Option<f32>,
    /// Formaldehyde in mg/m³. The wire field `hchr` is this × 1000.
    pub hcho_mg_m3: Option<f32>,
    pub hepa_life: Option<u8>,
    pub topic_prefix: Option<String>,
}

impl MachineState {
    /// The sensors read while the fan runs, or while it is off with
    /// continuous monitoring on. Otherwise their readings are stale.
    pub fn sensing(&self) -> bool {
        self.power_on || self.monitoring
    }
}

/// Apple Home's air-quality scale: 0 unknown, 1 excellent … 5 poor.
/// Takes the worst of the pollutants the HP09 actually measures.
pub fn air_quality(state: &MachineState) -> u8 {
    let mut level = 0u8;
    let consider = |level: &mut u8, band: u8| {
        if band > *level {
            *level = band;
        }
    };
    if let Some(pm) = state.pm25 {
        consider(
            &mut level,
            match pm {
                n if n <= 12.0 => 1,
                n if n <= 35.4 => 2,
                n if n <= 55.4 => 3,
                n if n <= 150.0 => 4,
                _ => 5,
            },
        );
    }
    if let Some(pm) = state.pm10 {
        consider(
            &mut level,
            match pm {
                n if n <= 20.0 => 1,
                n if n <= 50.0 => 2,
                n if n <= 100.0 => 3,
                n if n <= 150.0 => 4,
                _ => 5,
            },
        );
    }
    // VOC and NO₂ arrive as Dyson's own 0–10 index, not a density.
    if let Some(index) = state.voc_index.or(state.no2_index) {
        consider(
            &mut level,
            match index {
                n if n <= 2.0 => 1,
                n if n <= 4.0 => 2,
                n if n <= 6.0 => 3,
                n if n <= 8.0 => 4,
                _ => 5,
            },
        );
    }
    if let Some(other) = state.no2_index.filter(|_| state.voc_index.is_some()) {
        consider(
            &mut level,
            match other {
                n if n <= 2.0 => 1,
                n if n <= 4.0 => 2,
                n if n <= 6.0 => 3,
                n if n <= 8.0 => 4,
                _ => 5,
            },
        );
    }
    // WHO's 30-minute formaldehyde guideline is 0.1 mg/m³.
    if let Some(hcho) = state.hcho_mg_m3 {
        consider(
            &mut level,
            match hcho {
                n if n <= 0.03 => 1,
                n if n <= 0.08 => 2,
                n if n <= 0.10 => 3,
                n if n <= 0.30 => 4,
                _ => 5,
            },
        );
    }
    level
}

pub fn speed_to_percent(speed: u8) -> f32 {
    (speed.clamp(1, 10) as f32) * 10.0
}

pub fn percent_to_speed(percent: f32) -> u8 {
    (percent / 10.0).round().clamp(1.0, 10.0) as u8
}

pub fn kelvin_tenths_to_c(raw: f32) -> f32 {
    raw / 10.0 - 273.15
}

pub fn c_to_heat_target(celsius: f32) -> String {
    let kelvin = (celsius + 273.15).clamp(274.0, 310.0);
    format!("{:04}", (kelvin * 10.0).round() as i32)
}

fn field<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    let value = map.get(key)?;
    match value {
        Value::Array(items) => items.last(),
        other => Some(other),
    }
}

fn field_str(map: &Map<String, Value>, key: &str) -> Option<String> {
    let value = field(map, key)?;
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(true) => Some("ON".into()),
        Value::Bool(false) => Some("OFF".into()),
        _ => None,
    }
}

fn meaningful(raw: &str) -> Option<&str> {
    let text = raw.trim();
    match text {
        "" | "OFF" | "off" | "NONE" | "INIT" | "FAIL" | "INV" => None,
        other => Some(other),
    }
}

fn parse_f32(raw: &str) -> Option<f32> {
    meaningful(raw)?.parse().ok()
}

fn apply_machine_fields(state: &mut MachineState, map: &Map<String, Value>) {
    if let Some(power) = field_str(map, "fpwr") {
        state.power_on = power == "ON";
    }
    if let Some(auto) = field_str(map, "auto") {
        state.auto = auto == "ON";
    }
    if let Some(speed) = field_str(map, "fnsp") {
        if let Some(number) = meaningful(&speed).and_then(|text| text.parse::<u8>().ok()) {
            if (1..=10).contains(&number) {
                state.speed = Some(number);
            }
        }
    }
    if let Some(oson) = field_str(map, "oson") {
        state.oscillation_extended = matches!(oson.as_str(), "OION" | "OIOF");
        state.oscillating = matches!(oson.as_str(), "ON" | "OION");
    }
    if let Some(low) = field_str(map, "osal").and_then(|raw| raw.trim().parse().ok()) {
        state.oscillation_low = Some(low);
    }
    if let Some(high) = field_str(map, "osau").and_then(|raw| raw.trim().parse().ok()) {
        state.oscillation_high = Some(high);
    }
    if let Some(night) = field_str(map, "nmod") {
        state.night = night == "ON";
    }
    if let Some(focus) = field_str(map, "fdir") {
        state.focus = focus == "ON";
    }
    if let Some(monitor) = field_str(map, "rhtm") {
        state.monitoring = monitor == "ON";
    }
    if let Some(heat) = field_str(map, "hmod") {
        state.heat_on = heat == "HEAT";
    }
    if let Some(heating) = field_str(map, "hsta") {
        state.heating_now = heating == "HEAT";
    }
    if let Some(target) = field_str(map, "hmax").and_then(|raw| parse_f32(&raw)) {
        state.heat_target_c = Some(kelvin_tenths_to_c(target));
    }
    if let Some(life) = field_str(map, "hflr").and_then(|raw| parse_f32(&raw)) {
        state.hepa_life = Some(life.clamp(0.0, 100.0) as u8);
    }
    apply_environment_fields(state, map);
}

/// A sensor field: `None` when absent, so the last reading stands, and
/// `Some(None)` when the fan says it is not measuring (`OFF`, `INIT`).
fn reading(map: &Map<String, Value>, keys: &[&str]) -> Option<Option<f32>> {
    let raw = keys.iter().find_map(|key| field_str(map, key))?;
    Some(parse_f32(&raw))
}

fn apply_environment_fields(state: &mut MachineState, map: &Map<String, Value>) {
    if let Some(temp) = reading(map, &["tact"]) {
        state.temperature_c = temp.map(kelvin_tenths_to_c);
    }
    if let Some(humidity) = reading(map, &["hact"]) {
        state.humidity = humidity.filter(|humidity| (0.0..=100.0).contains(humidity));
    }
    if let Some(pm) = reading(map, &["p25r", "pm25"]) {
        state.pm25 = pm;
    }
    if let Some(pm) = reading(map, &["p10r", "pm10"]) {
        state.pm10 = pm;
    }
    if let Some(voc) = reading(map, &["va10"]) {
        state.voc_index = voc.map(|voc| voc / 10.0);
    }
    if let Some(no2) = reading(map, &["noxl"]) {
        state.no2_index = no2.map(|no2| no2 / 10.0);
    }
    if let Some(hcho) = reading(map, &["hchr", "hcho"]) {
        state.hcho_mg_m3 = hcho.map(|hcho| hcho / 1000.0);
    }
}

/// Fold one MQTT payload into `state`. Returns false when the message is not a state update.
pub fn apply_message(state: &mut MachineState, topic: &str, payload: &str) -> bool {
    if let Some(prefix) = topic.split('/').next() {
        if !prefix.is_empty() && prefix != state.topic_prefix.as_deref().unwrap_or("") {
            state.topic_prefix = Some(prefix.to_owned());
        }
    }
    let value: Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let Some(root) = value.as_object() else {
        return false;
    };
    let kind = root.get("msg").and_then(Value::as_str).unwrap_or("");
    match kind {
        "CURRENT-STATE" | "STATE-CHANGE" => {
            if let Some(fields) = root.get("product-state").and_then(Value::as_object) {
                apply_machine_fields(state, fields);
                return true;
            }
            false
        }
        "ENVIRONMENTAL-CURRENT-SENSOR-DATA" => {
            if let Some(fields) = root.get("data").and_then(Value::as_object) {
                apply_environment_fields(state, fields);
                return true;
            }
            false
        }
        _ => false,
    }
}

pub fn intent_fields(state: &MachineState, intent: &Intent) -> Vec<(&'static str, String)> {
    match intent {
        Intent::Power(true) => vec![("fpwr", "ON".into())],
        Intent::Power(false) => vec![("fpwr", "OFF".into())],
        Intent::Auto(true) => vec![("fpwr", "ON".into()), ("auto", "ON".into())],
        Intent::Auto(false) => vec![("auto", "OFF".into())],
        Intent::Speed(speed) => vec![
            ("fpwr", "ON".into()),
            ("auto", "OFF".into()),
            ("fnsp", format!("{:04}", (*speed).clamp(1, 10))),
        ],
        // Equal limits oscillate across nothing, so a range too narrow to
        // see becomes a 90° sweep. A wider one, set in the Dyson app, stays.
        Intent::Swing(true) => match sweep(state) {
            Some(width) if width >= MIN_SWEEP => oscillate(state, None),
            _ => oscillate(state, Some(DEFAULT_SWEEP)),
        },
        Intent::Swing(false) => {
            let off = if state.oscillation_extended {
                "OIOF"
            } else {
                "OFF"
            };
            vec![("oson", off.into())]
        }
        Intent::Sweep(width) => oscillate(state, Some(*width)),
        Intent::Night(true) => vec![("nmod", "ON".into())],
        Intent::Night(false) => vec![("nmod", "OFF".into())],
        Intent::Focus(true) => vec![("fdir", "ON".into())],
        Intent::Focus(false) => vec![("fdir", "OFF".into())],
        Intent::Monitor(true) => vec![("rhtm", "ON".into())],
        Intent::Monitor(false) => vec![("rhtm", "OFF".into())],
        // The fan heats only while it blows, so heat turns it on.
        Intent::Heat(true) => vec![("fpwr", "ON".into()), ("hmod", "HEAT".into())],
        Intent::Heat(false) => vec![("hmod", "OFF".into())],
        Intent::HeatTargetC(celsius) => {
            vec![
                ("fpwr", "ON".into()),
                ("hmod", "HEAT".into()),
                ("hmax", c_to_heat_target(*celsius)),
            ]
        }
    }
}

/// The sweeps the Dyson app offers, in degrees.
pub const SWEEPS: [u16; 4] = [45, 90, 180, 350];
const DEFAULT_SWEEP: u16 = 90;
/// The fan rejects a narrower range. Its limits sit in 5–355.
const MIN_SWEEP: u16 = 30;

/// Degrees between the oscillation limits.
pub fn sweep(state: &MachineState) -> Option<u16> {
    Some(
        state
            .oscillation_high?
            .saturating_sub(state.oscillation_low?),
    )
}

/// Oscillation on, with a new sweep of `width` around where the fan points.
fn oscillate(state: &MachineState, width: Option<u16>) -> Vec<(&'static str, String)> {
    let on = if state.oscillation_extended {
        "OION"
    } else {
        "ON"
    };
    let mut fields = vec![("fpwr", "ON".into()), ("oson", on.into())];
    if let Some(width) = width {
        let width = width.clamp(MIN_SWEEP, 350);
        let centre = match (state.oscillation_low, state.oscillation_high) {
            (Some(low), Some(high)) => (low + high) / 2,
            _ => 180,
        };
        let low = centre.saturating_sub(width / 2).clamp(5, 355 - width);
        fields.push(("ancp", "CUST".into()));
        fields.push(("osal", format!("{low:04}")));
        fields.push(("osau", format!("{:04}", low + width)));
    }
    fields
}

pub fn state_set_payload(fields: &[(&str, String)]) -> String {
    let mut data = Map::new();
    for (key, value) in fields {
        data.insert((*key).to_owned(), Value::String(value.clone()));
    }
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    serde_json::json!({
        "msg": "STATE-SET",
        "time": now,
        "mode-reason": "LAPP",
        "data": data,
    })
    .to_string()
}

pub fn request_state_payload() -> String {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    serde_json::json!({ "msg": "REQUEST-CURRENT-STATE", "time": now }).to_string()
}

pub fn request_environment_payload() -> String {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    serde_json::json!({
        "msg": "REQUEST-PRODUCT-ENVIRONMENT-CURRENT-SENSOR-DATA",
        "time": now,
    })
    .to_string()
}

pub fn status_topic(product_type: &str, serial: &str) -> String {
    format!("{product_type}/{serial}/status/current")
}

pub fn command_topic(product_type: &str, serial: &str) -> String {
    format!("{product_type}/{serial}/command")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_state_and_change_pairs() {
        let mut state = MachineState::default();
        let payload = r#"{
            "msg":"CURRENT-STATE",
            "product-state":{
                "fpwr":"ON","auto":"OFF","fnsp":"0004","oson":"OION",
                "nmod":"OFF","fdir":"ON","rhtm":"ON","hmod":"OFF","hsta":"OFF",
                "hmax":"2980","hflr":"0086"
            }
        }"#;
        assert!(apply_message(
            &mut state,
            "527K/ABC-UK-1/status/current",
            payload
        ));
        assert!(state.power_on);
        assert_eq!(state.speed, Some(4));
        assert!(state.oscillating && state.oscillation_extended);
        assert!(state.focus && state.monitoring);
        assert_eq!(state.hepa_life, Some(86));
        assert_eq!(state.topic_prefix.as_deref(), Some("527K"));
        let heat = kelvin_tenths_to_c(2980.0);
        assert!((state.heat_target_c.unwrap() - heat).abs() < 0.01);

        let change = r#"{"msg":"STATE-CHANGE","product-state":{"fnsp":["0004","0007"],"auto":["OFF","ON"]}}"#;
        apply_message(&mut state, "527K/ABC-UK-1/status/current", change);
        assert_eq!(state.speed, Some(7));
        assert!(state.auto);
    }

    #[test]
    fn parses_environment_and_rates_air() {
        let mut state = MachineState::default();
        let payload = r#"{
            "msg":"ENVIRONMENTAL-CURRENT-SENSOR-DATA",
            "data":{"tact":"2950","hact":"0041","pm25":"0008","pm10":"0012","va10":"0030","noxl":"0010","hchr":"0020"}
        }"#;
        assert!(apply_message(&mut state, "527E/SN/status/current", payload));
        assert!((state.temperature_c.unwrap() - 21.85).abs() < 0.02);
        assert_eq!(state.humidity, Some(41.0));
        assert_eq!(state.pm25, Some(8.0));
        assert_eq!(state.voc_index, Some(3.0));
        assert_eq!(state.hcho_mg_m3, Some(0.02));
        assert_eq!(air_quality(&state), 2); // VOC index 3 sits in "good"
    }

    #[test]
    fn poor_pm_wins() {
        let state = MachineState {
            pm25: Some(160.0),
            voc_index: Some(1.0),
            ..MachineState::default()
        };
        assert_eq!(air_quality(&state), 5);
    }

    #[test]
    fn speed_and_heat_commands() {
        let state = MachineState {
            oscillation_extended: true,
            ..MachineState::default()
        };
        let fields = intent_fields(&state, &Intent::Swing(false));
        assert_eq!(fields, vec![("oson", "OIOF".into())]);
        let fields = intent_fields(&state, &Intent::Speed(10));
        assert!(fields.iter().any(|(k, v)| *k == "fnsp" && v == "0010"));
        let fields = intent_fields(&state, &Intent::HeatTargetC(21.85));
        let hmax = fields
            .iter()
            .find(|(k, _)| *k == "hmax")
            .unwrap()
            .1
            .as_str();
        assert_eq!(hmax, "2950");
        assert!(fields.contains(&("fpwr", "ON".into())));
        let fields = intent_fields(&state, &Intent::Heat(true));
        assert_eq!(fields[0], ("fpwr", "ON".into()));
        let payload = state_set_payload(&fields);
        assert!(payload.contains("STATE-SET") && payload.contains("LAPP"));
    }

    #[test]
    fn oscillation_widens_a_range_that_sweeps_nothing() {
        let mut state = MachineState::default();
        let current = r#"{"msg":"CURRENT-STATE","product-state":{"oson":"OFF","osal":"0100","osau":"0100","ancp":"CUST"}}"#;
        assert!(apply_message(&mut state, "000X/SN/status/current", current));
        assert_eq!(
            (state.oscillation_low, state.oscillation_high),
            (Some(100), Some(100))
        );
        let fields = intent_fields(&state, &Intent::Swing(true));
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("oson"), Some("ON"));
        assert_eq!(get("ancp"), Some("CUST"));
        assert_eq!(get("osal"), Some("0055"));
        assert_eq!(get("osau"), Some("0145"));

        // A range set in the Dyson app is kept.
        state.oscillation_low = Some(45);
        state.oscillation_high = Some(315);
        let fields = intent_fields(&state, &Intent::Swing(true));
        assert!(!fields.iter().any(|(k, _)| *k == "osal" || *k == "osau"));

        // Near the end stops, the sweep stays inside 5–355.
        state.oscillation_low = Some(350);
        state.oscillation_high = Some(350);
        let fields = intent_fields(&state, &Intent::Swing(true));
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!((get("osal"), get("osau")), (Some("0265"), Some("0355")));

        // Off sends only oson.
        let fields = intent_fields(&state, &Intent::Swing(false));
        assert_eq!(fields, vec![("oson", "OFF".into())]);

        // A preset sweep keeps its centre and stays inside the limits.
        state.oscillation_low = Some(56);
        state.oscillation_high = Some(146);
        assert_eq!(sweep(&state), Some(90));
        let fields = intent_fields(&state, &Intent::Sweep(45));
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!((get("osal"), get("osau")), (Some("0079"), Some("0124")));
        let fields = intent_fields(&state, &Intent::Sweep(350));
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!((get("osal"), get("osau")), (Some("0005"), Some("0355")));
        assert_eq!(Intent::parse("swing", "180"), Some(Intent::Sweep(180)));
    }

    #[test]
    fn a_sensor_that_stops_measuring_clears_its_reading() {
        let mut state = MachineState {
            humidity: Some(40.0),
            pm25: Some(4.0),
            ..MachineState::default()
        };
        apply_message(
            &mut state,
            "000X/SN/status/current",
            r#"{"msg":"ENVIRONMENTAL-CURRENT-SENSOR-DATA","data":{"hact":"OFF","tact":"NONE"}}"#,
        );
        assert_eq!(state.humidity, None);
        assert!(state.temperature_c.is_none());
        // A field the message leaves out keeps its last reading.
        assert_eq!(state.pm25, Some(4.0));
    }
}
