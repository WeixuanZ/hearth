//! Readings over time: temperature, humidity, particles, and any other
//! `reading`.
//!
//! One CSV file per device, under `~/.config/hearth/series/`. `time` is Unix
//! seconds, UTC, aligned to the minute. The other columns are capability ids.
//! An empty cell means that reading was not being measured. A spreadsheet,
//! DuckDB or pandas can open the file as it is.
//!
//! The fan publishes faster than the room changes, so a minute holds one
//! sample: the last value seen in it, kept to the thousandth of the unit
//! (finer than these sensors). A minute is written when a value changes, and
//! at least every fifteen minutes while it is still being measured, so a flat
//! night does not fill the disk and a gap longer than that means the sensor
//! was off. Rows older than thirty days are dropped.

use crate::config;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, Once},
    time::{SystemTime, UNIX_EPOCH},
};

/// One sample per minute.
pub const INTERVAL_SECS: u64 = 60;
/// A steady reading is stored at least this often, so a gap means it stopped.
pub const HEARTBEAT_SECS: u64 = 15 * 60;
/// How long a sample is kept.
pub const RETAIN_SECS: u64 = 30 * 24 * 60 * 60;
const FLUSH_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

static LOG: Mutex<Option<Log>> = Mutex::new(None);

/// Loads the files and starts saving. Safe to call once, from `hearth run`.
pub fn start() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = match default_dir() {
            Ok(dir) => dir,
            Err(error) => {
                log::warn!("readings history is off: {error:#}");
                return;
            }
        };
        match Log::open(dir) {
            Ok(log) => *lock() = Some(log),
            Err(error) => {
                log::warn!("readings history is off: {error:#}");
                return;
            }
        }
        std::thread::Builder::new()
            .name("hearth-series".into())
            .spawn(|| loop {
                std::thread::sleep(FLUSH_EVERY);
                service(false);
            })
            .ok();
    });
}

/// Remembers the latest value of each active reading. Does nothing until
/// [`start`], so a test that publishes a snapshot does not write a file.
pub fn observe(device: &str, readings: &[(&str, f64)]) {
    if device.is_empty() || readings.is_empty() {
        return;
    }
    let now = unix_now();
    let mut guard = lock();
    let Some(log) = guard.as_mut() else {
        return;
    };
    for (capability, value) in readings {
        log.note(device, capability, *value, now);
    }
}

/// Samples of one reading since `since` (Unix seconds), including the minute
/// still open. Empty when nothing has been recorded.
pub fn samples(device: &str, capability: &str, since: u64) -> Vec<(u64, i32)> {
    let guard = lock();
    guard
        .as_ref()
        .map(|log| log.samples(device, capability, since))
        .unwrap_or_default()
}

/// Deletes a device's file, including from memory when this process records.
pub fn forget(device: &str) {
    let mut guard = lock();
    if let Some(log) = guard.as_mut() {
        log.forget(device);
        return;
    }
    drop(guard);
    if let Ok(path) = csv_path(device) {
        let _ = fs::remove_file(path);
    }
}

/// Where a device's CSV is, whether or not it exists yet.
pub fn csv_path(device: &str) -> Result<PathBuf> {
    Ok(default_dir()?.join(format!("{}.csv", encode_id(device))))
}

/// Saves everything, including the minute underway. Called as the process stops.
pub fn shutdown() {
    service(true);
}

/// History from the files, for when the bridge is not running. A device is
/// matched by its id only; names need the running bridge.
pub fn offline_document(query: &str, capability: Option<&str>, hours: u64) -> Result<Value> {
    let log = Log::open(default_dir()?)?;
    let id = log.match_id(query)?;
    let (hours, since) = window(hours, unix_now());
    let columns = match capability {
        Some(capability) => {
            let known = log
                .devices
                .get(&id)
                .is_some_and(|device| device.columns.iter().any(|column| column == capability));
            if !known {
                anyhow::bail!("no history for {capability}");
            }
            vec![capability.to_owned()]
        }
        None => log
            .devices
            .get(&id)
            .map(|device| device.columns.clone())
            .unwrap_or_default(),
    };
    let owned: Vec<(String, Vec<(u64, i32)>)> = columns
        .into_iter()
        .map(|capability| {
            let samples = log.samples(&id, &capability, since);
            (capability, samples)
        })
        .collect();
    let views: Vec<SeriesOut<'_>> = owned
        .iter()
        .map(|(capability, samples)| SeriesOut {
            capability,
            name: capability,
            unit: "",
            samples,
        })
        .collect();
    Ok(document(&id, &id, &log.file(&id), hours, since, &views))
}

/// The hours asked for, clamped to the retention window, and the Unix second
/// to read from.
pub fn window(hours: u64, now: u64) -> (u64, u64) {
    let hours = hours.clamp(1, RETAIN_SECS / 3600);
    let since = now.saturating_sub(hours.saturating_mul(3600));
    let earliest = now.saturating_sub(RETAIN_SECS);
    (hours, since.max(earliest))
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// The value as written in the CSV and the JSON: up to three decimal places.
pub fn format_value(milli: i32) -> String {
    let negative = milli < 0;
    let milli = milli.unsigned_abs();
    let whole = milli / 1000;
    let fraction = milli % 1000;
    let body = if fraction == 0 {
        format!("{whole}")
    } else {
        let digits = format!("{fraction:03}");
        format!("{whole}.{}", digits.trim_end_matches('0'))
    };
    if negative {
        format!("-{body}")
    } else {
        body
    }
}

/// Rounds to the thousandth of the unit. None when it is not a finite number
/// this file can store.
pub fn milli_of(value: f64) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    let scaled = (value * 1000.0).round();
    if !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&scaled) {
        return None;
    }
    Some(scaled as i32)
}

/// One reading, borrowed, for [`document`].
pub struct SeriesOut<'a> {
    pub capability: &'a str,
    pub name: &'a str,
    pub unit: &'a str,
    pub samples: &'a [(u64, i32)],
}

pub fn document(
    device: &str,
    name: &str,
    path: &Path,
    hours: u64,
    since: u64,
    series: &[SeriesOut<'_>],
) -> Value {
    json!({
        "device": device,
        "name": name,
        "path": path.display().to_string(),
        "interval": INTERVAL_SECS,
        "heartbeat": HEARTBEAT_SECS,
        "retain": RETAIN_SECS,
        "hours": hours,
        "since": since,
        "series": series.iter().map(series_json).collect::<Vec<_>>(),
    })
}

fn series_json(series: &SeriesOut<'_>) -> Value {
    json!({
        "capability": series.capability,
        "name": series.name,
        "unit": series.unit,
        "samples": series.samples.iter().map(|(time, milli)| json!({
            "t": time,
            "v": json_number(*milli),
        })).collect::<Vec<_>>(),
    })
}

fn json_number(milli: i32) -> Value {
    serde_json::from_str(&format_value(milli)).unwrap_or(Value::Null)
}

fn service(force: bool) {
    let now = unix_now();
    let mut guard = lock();
    let Some(log) = guard.as_mut() else {
        return;
    };
    log.maintenance(now, force);
    if let Err(error) = log.write_dirty() {
        log::warn!("could not save readings: {error:#}");
    }
}

fn lock() -> std::sync::MutexGuard<'static, Option<Log>> {
    LOG.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn default_dir() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("series"))
}

fn minute_start(unix: u64) -> u64 {
    unix - unix % INTERVAL_SECS
}

struct Log {
    dir: PathBuf,
    devices: BTreeMap<String, DeviceSeries>,
}

#[derive(Default)]
struct DeviceSeries {
    columns: Vec<String>,
    rows: Vec<Row>,
    /// The last row is the minute underway, and may not be written yet.
    open: bool,
    dirty: bool,
}

struct Row {
    time: u64,
    values: Vec<Option<i32>>,
}

impl Log {
    fn open(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        let mut log = Self {
            dir,
            devices: BTreeMap::new(),
        };
        for entry in fs::read_dir(&log.dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "csv") {
                if let Err(error) = log.load_file(&path) {
                    log::warn!("could not read {}: {error:#}", path.display());
                }
            }
        }
        Ok(log)
    }

    fn note(&mut self, device: &str, capability: &str, value: f64, unix: u64) {
        if device.is_empty() || capability.is_empty() {
            return;
        }
        let Some(milli) = milli_of(value) else {
            return;
        };
        self.devices
            .entry(device.to_owned())
            .or_default()
            .record(capability, milli, unix);
    }

    fn samples(&self, device: &str, capability: &str, since: u64) -> Vec<(u64, i32)> {
        let Some(series) = self.devices.get(device) else {
            return Vec::new();
        };
        let Some(index) = series
            .columns
            .iter()
            .position(|column| column == capability)
        else {
            return Vec::new();
        };
        series
            .rows
            .iter()
            .filter_map(|row| {
                if row.time < since {
                    return None;
                }
                row.values
                    .get(index)
                    .copied()
                    .flatten()
                    .map(|value| (row.time, value))
            })
            .collect()
    }

    fn forget(&mut self, device: &str) {
        self.devices.remove(device);
        let _ = fs::remove_file(self.file(device));
    }

    fn file(&self, device: &str) -> PathBuf {
        self.dir.join(format!("{}.csv", encode_id(device)))
    }

    fn match_id(&self, query: &str) -> Result<String> {
        let folded = query.to_lowercase();
        let ids: Vec<&str> = self.devices.keys().map(String::as_str).collect();
        let exact: Vec<&str> = ids
            .iter()
            .copied()
            .filter(|id| id.to_lowercase() == folded)
            .collect();
        let matches = if exact.len() == 1 {
            exact
        } else if exact.is_empty() {
            ids.iter()
                .copied()
                .filter(|id| id.to_lowercase().contains(&folded))
                .collect()
        } else {
            exact
        };
        match matches.as_slice() {
            [id] => Ok((*id).to_owned()),
            [] => Err(anyhow!(
                "no history for {query}. Start the bridge, or pass the device id"
            )),
            many => Err(anyhow!(
                "{query} matches {}. Be more specific",
                many.join(", ")
            )),
        }
    }

    fn maintenance(&mut self, now: u64, force: bool) {
        let minute = minute_start(now);
        let ids: Vec<String> = self.devices.keys().cloned().collect();
        for id in ids {
            let Some(device) = self.devices.get_mut(&id) else {
                continue;
            };
            let stale = device.open && device.rows.last().is_some_and(|row| row.time < minute);
            if device.open && (force || stale) {
                device.finish(force);
            }
            device.trim_to(minute);
        }
    }

    fn write_dirty(&mut self) -> Result<()> {
        let pending: Vec<String> = self
            .devices
            .iter()
            .filter(|(_, device)| device.dirty)
            .map(|(id, _)| id.clone())
            .collect();
        for id in pending {
            let Some(device) = self.devices.get(&id) else {
                continue;
            };
            let path = self.file(&id);
            match device.csv() {
                Some(body) => atomic_write(&path, &body)?,
                None => {
                    if path.exists() {
                        fs::remove_file(&path)?;
                    }
                }
            }
            if let Some(device) = self.devices.get_mut(&id) {
                device.dirty = false;
            }
        }
        Ok(())
    }

    fn load_file(&mut self, path: &Path) -> Result<()> {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("a readings file with no name")?;
        let id = decode_id(stem).context("a readings file with a bad name")?;
        let text = fs::read_to_string(path)?;
        let mut lines = text.lines();
        let header = parse_line(lines.next().unwrap_or(""));
        let Some(time_at) = header.iter().position(|field| field == "time") else {
            anyhow::bail!("no time column");
        };
        let mut columns = Vec::new();
        let mut indexes = Vec::new();
        for (index, field) in header.iter().enumerate() {
            if index == time_at || field.is_empty() || columns.iter().any(|column| column == field)
            {
                continue;
            }
            columns.push(field.clone());
            indexes.push(index);
        }
        let mut by_time = BTreeMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let fields = parse_line(line);
            let Some(time) = fields.get(time_at).and_then(|text| text.parse().ok()) else {
                continue;
            };
            let values = indexes
                .iter()
                .map(|index| fields.get(*index).and_then(|text| parse_milli(text)))
                .collect();
            by_time.insert(time, values);
        }
        let rows = by_time
            .into_iter()
            .map(|(time, values)| Row { time, values })
            .collect();
        self.devices.insert(
            id,
            DeviceSeries {
                columns,
                rows,
                open: false,
                dirty: false,
            },
        );
        Ok(())
    }
}

impl DeviceSeries {
    fn record(&mut self, capability: &str, milli: i32, unix: u64) {
        let minute = minute_start(unix);
        let column = self.column(capability);
        if self.rows.last().is_some_and(|row| row.time == minute) {
            self.open = true;
            self.set(column, milli);
            return;
        }
        if self.open {
            self.finish(false);
        }
        let mut values = vec![None; self.columns.len()];
        values[column] = Some(milli);
        self.rows.push(Row {
            time: minute,
            values,
        });
        self.open = true;
        self.dirty = true;
    }

    fn column(&mut self, name: &str) -> usize {
        if let Some(index) = self.columns.iter().position(|column| column == name) {
            return index;
        }
        self.columns.push(name.to_owned());
        for row in &mut self.rows {
            row.values.push(None);
        }
        self.dirty = true;
        self.columns.len() - 1
    }

    fn set(&mut self, column: usize, milli: i32) {
        let Some(row) = self.rows.last_mut() else {
            return;
        };
        if row.values.get(column).copied().flatten() == Some(milli) {
            return;
        }
        if column >= row.values.len() {
            row.values.resize(column + 1, None);
        }
        row.values[column] = Some(milli);
        self.dirty = true;
    }

    fn finish(&mut self, force: bool) {
        if !self.open {
            return;
        }
        let has_value = self
            .rows
            .last()
            .is_some_and(|row| row.values.iter().any(Option::is_some));
        let keep = has_value && (force || self.worth_keeping());
        self.open = false;
        if !keep {
            self.rows.pop();
            self.dirty = true;
            return;
        }
        if let Some(newest) = self.rows.last().map(|row| row.time) {
            self.trim_to(newest);
        }
    }

    fn worth_keeping(&self) -> bool {
        let Some(open) = self.rows.last() else {
            return false;
        };
        let Some(previous) = self.rows.iter().rev().nth(1) else {
            return true;
        };
        open.time.saturating_sub(previous.time) >= HEARTBEAT_SECS || open.values != previous.values
    }

    fn trim_to(&mut self, newest: u64) {
        let cutoff = newest.saturating_sub(RETAIN_SECS);
        let end = if self.open {
            self.rows.len().saturating_sub(1)
        } else {
            self.rows.len()
        };
        let drop = self.rows[..end]
            .iter()
            .position(|row| row.time >= cutoff)
            .unwrap_or(end);
        if drop > 0 {
            self.rows.drain(0..drop);
            self.dirty = true;
        }
    }

    /// Rows that belong in the file. The open minute is left out when it
    /// repeats the previous one and the heartbeat has not elapsed.
    fn persisted(&self) -> &[Row] {
        match self.rows.split_last() {
            Some((_, earlier)) if self.open && !self.worth_keeping() => earlier,
            _ => &self.rows,
        }
    }

    fn csv(&self) -> Option<String> {
        let rows = self.persisted();
        if rows.is_empty() {
            return None;
        }
        let mut out = String::from("time");
        for column in &self.columns {
            out.push(',');
            out.push_str(&escape(column));
        }
        out.push('\n');
        for row in rows {
            out.push_str(&row.time.to_string());
            for value in &row.values {
                out.push(',');
                if let Some(value) = value {
                    out.push_str(&format_value(*value));
                }
            }
            out.push('\n');
        }
        Some(out)
    }
}

fn encode_id(id: &str) -> String {
    let mut out = String::new();
    for byte in id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn decode_id(stem: &str) -> Option<String> {
    let bytes = stem.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn parse_milli(text: &str) -> Option<i32> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    milli_of(text.parse().ok()?)
}

fn parse_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(char) = chars.next() {
        match char {
            '"' if !quoted => quoted = true,
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' if quoted => quoted = false,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            _ => field.push(char),
        }
    }
    fields.push(field);
    fields
}

fn escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

fn atomic_write(path: &Path, body: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = path.with_extension("csv.tmp");
    {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path).with_context(|| format!("could not save {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_log() -> (Log, PathBuf) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("hearth-series-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let log = Log::open(dir.clone()).unwrap();
        (log, dir)
    }

    #[test]
    fn a_minute_keeps_the_last_value_and_rounds_to_a_thousandth() {
        let (mut log, dir) = temp_log();
        log.note("dyson:ABC", "temperature", 21.4, 10);
        log.note("dyson:ABC", "temperature", 21.8504, 50);
        log.note("dyson:ABC", "humidity", 41.0, 50);
        let samples = log.samples("dyson:ABC", "temperature", 0);
        assert_eq!(samples, vec![(0, 21850)]);
        assert_eq!(format_value(21850), "21.85");
        assert_eq!(format_value(-500), "-0.5");
        assert_eq!(format_value(21000), "21");
        log.write_dirty().unwrap();
        let text = fs::read_to_string(dir.join("dyson%3AABC.csv")).unwrap();
        assert_eq!(text, "time,temperature,humidity\n0,21.85,41\n");
        let loaded = Log::open(dir).unwrap();
        assert_eq!(loaded.samples("dyson:ABC", "humidity", 0), vec![(0, 41000)]);
        let _ = fs::remove_dir_all(loaded.dir);
    }

    #[test]
    fn an_unchanged_minute_is_not_stored_but_a_change_and_a_heartbeat_are() {
        let (mut log, dir) = temp_log();
        log.note("dyson:ABC", "pm25", 4.0, 0);
        log.note("dyson:ABC", "pm25", 4.0, 60);
        log.write_dirty().unwrap();
        let text = fs::read_to_string(dir.join("dyson%3AABC.csv")).unwrap();
        assert_eq!(text, "time,pm25\n0,4\n");
        // Still in memory, so a chart drawn now reaches the current minute.
        assert_eq!(
            log.samples("dyson:ABC", "pm25", 0),
            vec![(0, 4000), (60, 4000)]
        );

        log.note("dyson:ABC", "pm25", 9.0, 120);
        log.note("dyson:ABC", "pm25", 9.0, 120 + HEARTBEAT_SECS);
        log.write_dirty().unwrap();
        let loaded = Log::open(dir).unwrap();
        assert_eq!(
            loaded.samples("dyson:ABC", "pm25", 0),
            vec![(0, 4000), (120, 9000), (120 + HEARTBEAT_SECS, 9000)]
        );
        let _ = fs::remove_dir_all(loaded.dir);
    }

    #[test]
    fn rows_older_than_the_retention_window_are_dropped() {
        let mut device = DeviceSeries {
            columns: vec!["temperature".into()],
            rows: vec![
                Row {
                    time: 0,
                    values: vec![Some(1000)],
                },
                Row {
                    time: 31 * 24 * 60 * 60,
                    values: vec![Some(2000)],
                },
            ],
            ..DeviceSeries::default()
        };
        device.trim_to(31 * 24 * 60 * 60);
        assert_eq!(device.rows.len(), 1);
        assert_eq!(device.rows[0].time, 31 * 24 * 60 * 60);
    }

    #[test]
    fn a_slash_in_an_id_cannot_escape_the_directory() {
        let (mut log, dir) = temp_log();
        log.note("frame:a/b", "temperature", 20.0, 0);
        log.write_dirty().unwrap();
        let name = dir.join("frame%3Aa%2Fb.csv");
        assert!(name.is_file());
        assert!(!dir.join("frame:a").exists());
        let loaded = Log::open(dir).unwrap();
        assert_eq!(loaded.match_id("a/b").unwrap(), "frame:a/b");
        let _ = fs::remove_dir_all(loaded.dir);
    }

    #[test]
    fn quoted_csv_from_a_spreadsheet_round_trips() {
        let (log, dir) = temp_log();
        let path = dir.join("dyson%3AABC.csv");
        fs::write(&path, "time,\"pm2,5\"\n60,\"3.5\"\n").unwrap();
        let loaded = Log::open(dir).unwrap();
        assert_eq!(loaded.samples("dyson:ABC", "pm2,5", 0), vec![(60, 3500)]);
        drop(log);
        let _ = fs::remove_dir_all(loaded.dir);
    }
}
