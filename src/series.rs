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
//! was off. A minute is appended once it ends, so the file only grows at the
//! end; rows older than thirty days are dropped once a day.
//!
//! `format.json` beside the files says which layout they are in. A hearth
//! that finds a newer one leaves the files alone and records nothing, and
//! two computers share history only when their formats match. A change an
//! older hearth would misread raises [`FORMAT`] and migrates in `Log::open`.

use crate::config;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, Once},
    time::{SystemTime, UNIX_EPOCH},
};

/// The layout of the files, as `format.json` records it. 1 is the layout
/// above, which is also what a folder from before `format.json` holds.
pub const FORMAT: u64 = 1;
const MANIFEST: &str = "format.json";

/// One sample per minute.
pub const INTERVAL_SECS: u64 = 60;
/// A steady reading is stored at least this often, so a gap means it stopped.
pub const HEARTBEAT_SECS: u64 = 15 * 60;
/// How long a sample is kept.
pub const RETAIN_SECS: u64 = 30 * 24 * 60 * 60;
/// Old rows are dropped in batches this long, so files are rarely rewritten.
const COMPACT_SECS: u64 = 24 * 60 * 60;
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

/// One device's rows as one computer hands them to another: the minute,
/// and each reading by name.
pub type Rows = Vec<(u64, BTreeMap<String, i32>)>;

/// A hash of each day of each device's history, by device.
pub type DayHashes = BTreeMap<String, BTreeMap<u64, String>>;

/// A hash of each day of each device's history, minutes that have ended
/// only, so two computers can tell which days they hold differently.
/// Marked with the format, so neither takes rows it would misread.
pub fn digest() -> Value {
    let devices: DayHashes = lock().as_ref().map_or_else(BTreeMap::new, |log| {
        log.devices
            .iter()
            .map(|(id, device)| (id.clone(), device.digest()))
            .collect()
    });
    json!({ "format": FORMAT, "devices": devices })
}

/// Another computer's [`digest`], if its files are in this format. One from
/// before formats were marked is a bare map, in format 1.
pub fn their_digest(reply: Value) -> Result<DayHashes> {
    let (format, devices) = match reply.get("format") {
        Some(format) => (format.as_u64(), reply["devices"].clone()),
        None => (Some(1), reply),
    };
    if format != Some(FORMAT) {
        anyhow::bail!(
            "its readings are in format {}, ours in {FORMAT}",
            format.map_or("unknown".into(), |format| format.to_string())
        );
    }
    Ok(serde_json::from_value(devices)?)
}

/// A device's rows on the given days.
pub fn rows(device: &str, days: &[u64]) -> Rows {
    lock()
        .as_ref()
        .and_then(|log| log.devices.get(device))
        .map(|series| series.export(days))
        .unwrap_or_default()
}

/// Takes in another computer's rows. A minute either lacks is filled; one
/// both hold keeps the larger of two values. That does not depend on the
/// order or the repetition of merges, so every computer ends with the same.
pub fn merge(device: &str, rows: Rows) {
    if let Some(log) = lock().as_mut() {
        log.devices
            .entry(device.to_owned())
            .or_default()
            .merge(rows, unix_now());
    }
}

/// Deletes the history of removed devices, as removals reach this computer.
pub fn forget_removed<'a>(removed: impl IntoIterator<Item = &'a String>) {
    let mut guard = lock();
    let Some(log) = guard.as_mut() else {
        return;
    };
    for device in removed {
        if log.devices.contains_key(device) {
            log.forget(device);
        }
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

/// One hour of a reading's usual day: the range most days stayed within
/// then, from their tenth to their ninetieth percentile, and their mean.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Normal {
    pub hour: u32,
    pub low: f64,
    pub mean: f64,
    pub high: f64,
}

/// The usual day of a reading, from the days before `today` (its first
/// second, Unix). `hour_of` gives a moment's hour on the local clock. An
/// hour measured on fewer than three days is left out, as is today. Also
/// returns how many days it drew on.
pub fn normals(
    samples: &[(u64, i32)],
    today: u64,
    hour_of: impl Fn(u64) -> u32,
) -> (Vec<Normal>, usize) {
    const DAY: u64 = 24 * 60 * 60;
    // An hour of one day: the mean of what was stored in it.
    let mut hours: BTreeMap<(u64, u32), (f64, u32)> = BTreeMap::new();
    for &(time, milli) in samples.iter().filter(|(time, _)| *time < today) {
        let day = (today - 1 - time) / DAY;
        let slot = hours.entry((day, hour_of(time))).or_default();
        slot.0 += f64::from(milli) / 1000.0;
        slot.1 += 1;
    }
    let days = hours
        .keys()
        .map(|(day, _)| day)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let mut by_hour: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
    for ((_, hour), (sum, count)) in hours {
        by_hour
            .entry(hour)
            .or_default()
            .push(sum / f64::from(count));
    }
    let normals = by_hour
        .into_iter()
        .filter(|(_, values)| values.len() >= 3)
        .map(|(hour, mut values)| {
            values.sort_by(f64::total_cmp);
            Normal {
                hour,
                low: percentile(&values, 0.1),
                mean: values.iter().sum::<f64>() / values.len() as f64,
                high: percentile(&values, 0.9),
            }
        })
        .collect();
    (normals, days)
}

/// Linear between the two nearest of `sorted`.
fn percentile(sorted: &[f64], share: f64) -> f64 {
    let at = share * (sorted.len() - 1) as f64;
    let (below, above) = (at.floor() as usize, at.ceil() as usize);
    sorted[below] + (sorted[above] - sorted[below]) * (at - at.floor())
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
    if let Err(error) = log.write() {
        log::warn!("could not save readings: {error:#}");
    }
}

fn lock() -> std::sync::MutexGuard<'static, Option<Log>> {
    LOG.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Refuses a folder in a newer format, and marks one with none: that is a
/// folder from before formats were recorded, which is format 1.
fn check_format(dir: &Path) -> Result<()> {
    let path = dir.join(MANIFEST);
    let found = match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Value>(&text)?["format"]
            .as_u64()
            .context("format.json does not say a format")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::write(&path, format!("{}\n", json!({ "format": FORMAT })))?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    match found {
        FORMAT => Ok(()),
        newer if newer > FORMAT => Err(anyhow!(
            "{} holds readings in format {newer}, newer than this hearth reads ({FORMAT}). \
             They are left as they are; update hearth to keep recording",
            dir.display()
        )),
        older => Err(anyhow!("no migration from readings format {older}")),
    }
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
    /// The last row is the minute underway. It stays in memory until it ends.
    open: bool,
    /// How many rows, from the first, the file already holds.
    written: usize,
    /// The file no longer matches the rows before `written`: a column was
    /// added, or old rows dropped. Written whole next time.
    rewrite: bool,
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
        check_format(&dir)?;
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
            .filter(|row| row.time >= since)
            .filter_map(|row| Some((row.time, row.values.get(index).copied().flatten()?)))
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
        let matches = if exact.is_empty() {
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

    /// Ends minutes that are over, all of them when `force`, and drops what
    /// is older than the retention window.
    fn maintenance(&mut self, now: u64, force: bool) {
        let minute = minute_start(now);
        for device in self.devices.values_mut() {
            let ended = device.rows.last().is_some_and(|row| row.time < minute);
            if device.open && (force || ended) {
                device.finish(force);
            }
            device.compact(now);
        }
    }

    /// Appends the minutes that have ended to each file, or writes it whole
    /// when it has to change.
    fn write(&mut self) -> Result<()> {
        for (id, device) in &mut self.devices {
            let path = self.dir.join(format!("{}.csv", encode_id(id)));
            device.save(&path)?;
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
        for line in lines.filter(|line| !line.is_empty()) {
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
        let rows: Vec<Row> = by_time
            .into_iter()
            .map(|(time, values)| Row { time, values })
            .collect();
        // A file edited by hand, or out of order, is written back as read.
        let rewrite = header.first().map(String::as_str) != Some("time")
            || header.len() != columns.len() + 1
            || text.lines().skip(1).filter(|line| !line.is_empty()).count() != rows.len();
        self.devices.insert(
            id,
            DeviceSeries {
                columns,
                written: rows.len(),
                rows,
                open: false,
                rewrite,
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
            // A minute already in the file, as after a quick restart, changes.
            if !self.open && self.written == self.rows.len() {
                self.written -= 1;
                self.rewrite = true;
            }
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
    }

    fn column(&mut self, name: &str) -> usize {
        if let Some(index) = self.columns.iter().position(|column| column == name) {
            return index;
        }
        self.columns.push(name.to_owned());
        for row in &mut self.rows {
            row.values.push(None);
        }
        self.rewrite = true;
        self.columns.len() - 1
    }

    fn set(&mut self, column: usize, milli: i32) {
        if let Some(row) = self.rows.last_mut() {
            row.values.resize(row.values.len().max(column + 1), None);
            row.values[column] = Some(milli);
        }
    }

    /// Ends the minute underway: kept if it says something new, or the
    /// heartbeat is due, or `force` (the process is stopping).
    fn finish(&mut self, force: bool) {
        if !self.open {
            return;
        }
        self.open = false;
        let has_value = self
            .rows
            .last()
            .is_some_and(|row| row.values.iter().any(Option::is_some));
        if !(has_value && (force || self.worth_keeping())) {
            self.rows.pop();
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

    /// Drops rows past the retention window, a day's worth at a time, so the
    /// file is rewritten about once a day rather than every minute.
    fn compact(&mut self, now: u64) {
        let cutoff = now.saturating_sub(RETAIN_SECS);
        let stale = |row: &Row| row.time < cutoff;
        if self
            .rows
            .first()
            .is_none_or(|row| row.time + COMPACT_SECS >= cutoff)
        {
            return;
        }
        let drop = self
            .rows
            .iter()
            .take(self.rows.len() - usize::from(self.open))
            .take_while(|row| stale(row))
            .count();
        self.rows.drain(..drop);
        self.written = self.written.saturating_sub(drop);
        self.rewrite = true;
    }

    fn save(&mut self, path: &Path) -> Result<()> {
        let ended = self.rows.len() - usize::from(self.open);
        if self.rewrite || self.written == 0 {
            if ended == 0 {
                let _ = fs::remove_file(path);
            } else {
                atomic_write(path, &self.csv(&self.rows[..ended]))?;
            }
            self.rewrite = false;
        } else if ended > self.written {
            let mut body = String::new();
            for row in &self.rows[self.written..ended] {
                body.push_str(&line(row));
            }
            append(path, &body)?;
        }
        self.written = ended;
        Ok(())
    }

    fn ended(&self) -> &[Row] {
        &self.rows[..self.rows.len() - usize::from(self.open)]
    }

    /// Each row's cells by name, in name order, so the column order of a
    /// file does not matter.
    fn named(&self, row: &Row) -> BTreeMap<String, i32> {
        self.columns
            .iter()
            .zip(&row.values)
            .filter_map(|(column, value)| Some((column.clone(), (*value)?)))
            .collect()
    }

    fn digest(&self) -> BTreeMap<u64, String> {
        let mut days: BTreeMap<u64, Sha256> = BTreeMap::new();
        for row in self.ended() {
            let hash = days.entry(day(row.time)).or_default();
            hash.update(row.time.to_be_bytes());
            for (column, value) in self.named(row) {
                hash.update(column.as_bytes());
                hash.update([0]);
                hash.update(value.to_be_bytes());
            }
        }
        days.into_iter()
            .map(|(day, hash)| (day, format!("{:x}", hash.finalize())))
            .collect()
    }

    fn export(&self, days: &[u64]) -> Rows {
        self.ended()
            .iter()
            .filter(|row| days.contains(&day(row.time)))
            .map(|row| (row.time, self.named(row)))
            .collect()
    }

    fn merge(&mut self, rows: Rows, now: u64) {
        let cutoff = now.saturating_sub(RETAIN_SECS);
        // The minute underway stays last; nothing after it comes in.
        let open = self
            .open
            .then(|| self.rows.last().map(|row| row.time))
            .flatten();
        let mut changed = false;
        for (time, cells) in rows {
            if time < cutoff || open.is_some_and(|open| time > open) {
                continue;
            }
            let columns: Vec<(usize, i32)> = cells
                .into_iter()
                .map(|(name, value)| (self.column(&name), value))
                .collect();
            let at = match self.rows.binary_search_by_key(&time, |row| row.time) {
                Ok(at) => at,
                Err(at) => {
                    let values = vec![None; self.columns.len()];
                    self.rows.insert(at, Row { time, values });
                    changed = true;
                    at
                }
            };
            let row = &mut self.rows[at];
            row.values.resize(self.columns.len(), None);
            for (column, value) in columns {
                let cell = &mut row.values[column];
                if cell.is_none_or(|have| have < value) {
                    *cell = Some(value);
                    changed = true;
                }
            }
        }
        self.rewrite |= changed;
    }

    fn header(&self) -> String {
        let mut out = String::from("time");
        for column in &self.columns {
            out.push(',');
            out.push_str(&escape(column));
        }
        out.push('\n');
        out
    }

    fn csv(&self, rows: &[Row]) -> String {
        let mut out = self.header();
        for row in rows {
            out.push_str(&line(row));
        }
        out
    }
}

fn day(time: u64) -> u64 {
    time / (24 * 60 * 60)
}

fn line(row: &Row) -> String {
    let mut out = row.time.to_string();
    for value in &row.values {
        out.push(',');
        if let Some(value) = value {
            out.push_str(&format_value(*value));
        }
    }
    out.push('\n');
    out
}

fn append(path: &Path, body: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(body.as_bytes()))
        .with_context(|| format!("could not save {}", path.display()))
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
mod normal_tests {
    use super::*;

    #[test]
    fn a_usual_day_is_each_hours_range_across_the_days_before_today() {
        const DAY: u64 = 86_400;
        let today = 10 * DAY;
        let hour_of = |time: u64| (time % DAY / 3600) as u32;
        // Five days at 9:00 read 18 to 22 °C, two days measured 15:00, and
        // today, which is not part of the usual day, reads 30 °C.
        let mut samples: Vec<(u64, i32)> = (1..=5)
            .map(|ago| (today - ago * DAY + 9 * 3600, 17_000 + ago as i32 * 1000))
            .collect();
        samples.extend((1..=2).map(|ago| (today - ago * DAY + 15 * 3600, 25_000)));
        samples.push((today + 9 * 3600, 30_000));
        let (normals, days) = normals(&samples, today, hour_of);
        assert_eq!(days, 5);
        assert_eq!(normals.len(), 1, "15:00 has too few days");
        let nine = &normals[0];
        assert_eq!(nine.hour, 9);
        assert!((nine.mean - 20.0).abs() < 1e-9);
        assert!((nine.low - 18.4).abs() < 1e-9 && (nine.high - 21.6).abs() < 1e-9);
    }
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
    fn a_folder_records_its_format_and_a_newer_one_is_left_alone() {
        let (_, dir) = temp_log();
        let manifest = dir.join(MANIFEST);
        let written: Value = serde_json::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(written["format"], FORMAT);

        fs::write(&manifest, json!({ "format": FORMAT + 1 }).to_string()).unwrap();
        fs::write(dir.join("abc%3A1.csv"), "time,temperature\n60,20\n").unwrap();
        let refused = Log::open(dir.clone()).err().unwrap().to_string();
        assert!(refused.contains("newer"));
        assert_eq!(
            fs::read_to_string(dir.join("abc%3A1.csv")).unwrap(),
            "time,temperature\n60,20\n"
        );
    }

    #[test]
    fn history_is_shared_only_in_the_same_format() {
        let days = json!({ "abc:1": { "19000": "hash" } });
        let ours = their_digest(json!({ "format": FORMAT, "devices": days })).unwrap();
        assert_eq!(ours["abc:1"][&19000], "hash");
        // Before formats were marked: a bare map, which is format 1.
        assert_eq!(their_digest(days.clone()).unwrap(), ours);
        let newer = json!({ "format": FORMAT + 1, "devices": days });
        assert!(their_digest(newer).is_err());
    }

    #[test]
    fn a_minute_keeps_the_last_value_and_rounds_to_a_thousandth() {
        let (mut log, dir) = temp_log();
        log.note("dyson:ABC", "temperature", 21.4, 10);
        log.note("dyson:ABC", "temperature", 21.8504, 50);
        log.note("dyson:ABC", "humidity", 41.0, 50);
        // The minute underway is in memory, and written once it ends.
        log.write().unwrap();
        assert!(!dir.join("dyson%3AABC.csv").exists());
        log.maintenance(60, false);
        let samples = log.samples("dyson:ABC", "temperature", 0);
        assert_eq!(samples, vec![(0, 21850)]);
        assert_eq!(format_value(21850), "21.85");
        assert_eq!(format_value(-500), "-0.5");
        assert_eq!(format_value(21000), "21");
        log.write().unwrap();
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
        log.write().unwrap();
        let text = fs::read_to_string(dir.join("dyson%3AABC.csv")).unwrap();
        assert_eq!(text, "time,pm25\n0,4\n");
        // Still in memory, so a chart drawn now reaches the current minute.
        assert_eq!(
            log.samples("dyson:ABC", "pm25", 0),
            vec![(0, 4000), (60, 4000)]
        );

        log.note("dyson:ABC", "pm25", 9.0, 120);
        log.note("dyson:ABC", "pm25", 9.0, 120 + HEARTBEAT_SECS);
        // As when the process stops: the minute underway is kept too.
        log.maintenance(120 + HEARTBEAT_SECS, true);
        log.write().unwrap();
        let text = fs::read_to_string(dir.join("dyson%3AABC.csv")).unwrap();
        assert_eq!(
            text,
            format!("time,pm25\n0,4\n120,9\n{},9\n", 120 + HEARTBEAT_SECS)
        );
        let loaded = Log::open(dir).unwrap();
        assert_eq!(
            loaded.samples("dyson:ABC", "pm25", 0),
            vec![(0, 4000), (120, 9000), (120 + HEARTBEAT_SECS, 9000)]
        );
        let _ = fs::remove_dir_all(loaded.dir);
    }

    #[test]
    fn old_rows_are_dropped_a_day_at_a_time() {
        let day = 24 * 60 * 60;
        let mut device = DeviceSeries {
            columns: vec!["temperature".into()],
            rows: vec![
                Row {
                    time: 0,
                    values: vec![Some(1000)],
                },
                Row {
                    time: 2 * day,
                    values: vec![Some(2000)],
                },
            ],
            written: 2,
            ..DeviceSeries::default()
        };
        // A few hours past the window: kept until a day's worth is due.
        device.compact(RETAIN_SECS + 3600);
        assert_eq!((device.rows.len(), device.rewrite), (2, false));
        device.compact(RETAIN_SECS + day + 3600);
        assert_eq!(device.rows.len(), 1);
        assert_eq!(
            (device.rows[0].time, device.written, device.rewrite),
            (2 * day, 1, true)
        );
    }

    #[test]
    fn ended_minutes_are_appended_and_a_new_column_rewrites() {
        let (mut log, dir) = temp_log();
        let path = dir.join("dyson%3AABC.csv");
        log.note("dyson:ABC", "pm25", 4.0, 0);
        log.note("dyson:ABC", "pm25", 5.0, 60);
        log.note("dyson:ABC", "pm25", 6.0, 120);
        log.write().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "time,pm25\n0,4\n60,5\n");
        log.note("dyson:ABC", "humidity", 40.0, 120);
        log.note("dyson:ABC", "pm25", 7.0, 180);
        log.write().unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "time,pm25,humidity\n0,4,\n60,5,\n120,6,40\n"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_slash_in_an_id_cannot_escape_the_directory() {
        let (mut log, dir) = temp_log();
        log.note("frame:a/b", "temperature", 20.0, 0);
        log.maintenance(0, true);
        log.write().unwrap();
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

    #[test]
    fn two_computers_end_with_the_same_history_whichever_merges_first() {
        let (mut one, one_dir) = temp_log();
        let (mut other, other_dir) = temp_log();
        // The first saw the morning, the second the afternoon, both noon.
        for minute in 0..3 {
            one.note("abc:1", "temperature", 20.0 + minute as f64, minute * 60);
        }
        for minute in 2..5 {
            other.note("abc:1", "humidity", 40.0 + minute as f64, minute * 60);
        }
        one.maintenance(600, true);
        other.maintenance(600, true);
        let digest = |log: &Log| log.devices["abc:1"].digest();
        assert_ne!(digest(&one), digest(&other));

        let days: Vec<u64> = digest(&other).keys().copied().collect();
        let theirs = other.devices["abc:1"].export(&days);
        let ours = one.devices["abc:1"].export(&days);
        one.devices
            .get_mut("abc:1")
            .unwrap()
            .merge(theirs.clone(), 600);
        other.devices.get_mut("abc:1").unwrap().merge(ours, 600);
        assert_eq!(digest(&one), digest(&other));
        assert_eq!(one.samples("abc:1", "temperature", 0).len(), 3);
        assert_eq!(
            other.samples("abc:1", "humidity", 120),
            vec![(120, 42000), (180, 43000), (240, 44000)]
        );

        // Merging again changes nothing, and the files say the same.
        one.devices.get_mut("abc:1").unwrap().merge(theirs, 600);
        assert!(one.devices["abc:1"].rewrite);
        one.write().unwrap();
        other.write().unwrap();
        let file = |dir: &PathBuf| fs::read_to_string(dir.join("abc%3A1.csv")).unwrap();
        assert_eq!(
            Log::open(one_dir.clone()).unwrap().devices["abc:1"].digest(),
            Log::open(other_dir.clone()).unwrap().devices["abc:1"].digest()
        );
        assert!(file(&one_dir).starts_with("time,"));
        let _ = fs::remove_dir_all(one_dir);
        let _ = fs::remove_dir_all(other_dir);
    }
}
