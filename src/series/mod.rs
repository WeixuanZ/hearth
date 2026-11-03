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
};

mod query;
mod store;

pub use query::*;
use store::*;

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
        // Readings come from the devices' state, as they publish it.
        let mut state = crate::bus::watch();
        tokio::spawn(async move {
            let mut seen = crate::bus::Devices::new();
            loop {
                let devices = state.borrow_and_update().clone();
                for (id, device) in devices.iter() {
                    if !seen
                        .get(id)
                        .is_some_and(|was| std::sync::Arc::ptr_eq(was, device))
                    {
                        observe_snapshot(device);
                    }
                }
                seen = (*devices).clone();
                if state.changed().await.is_err() {
                    break;
                }
            }
        });
    });
}

/// A snapshot's active readings, as [`observe`] takes them.
fn observe_snapshot(snapshot: &crate::capabilities::Snapshot) {
    let readings: Vec<(&str, f64)> = snapshot
        .capabilities
        .iter()
        .filter(|capability| {
            matches!(capability.kind, crate::capabilities::Kind::Reading { .. })
                && capability.active
        })
        .filter_map(|capability| Some((capability.id, capability.value.as_f64()?)))
        .collect();
    observe(&snapshot.id, &readings);
}

/// Remembers the latest value of each active reading. Does nothing until
/// [`start`], so a test that publishes a snapshot does not write a file.
pub fn observe(device: &str, readings: &[(&str, f64)]) {
    if device.is_empty() || readings.is_empty() {
        return;
    }
    let now = crate::clock::now();
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
            .merge(rows, crate::clock::now());
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
    let (hours, since) = window(hours, crate::clock::now());
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

fn service(force: bool) {
    let now = crate::clock::now();
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
