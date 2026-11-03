//! The files themselves: one CSV a device, a row a minute, appended as
//! minutes end and compacted once a day.

use super::*;

/// Refuses a folder in a newer format, and marks one with none: that is a
/// folder from before formats were recorded, which is format 1.
pub(super) fn check_format(dir: &Path) -> Result<()> {
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
pub(super) fn default_dir() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("series"))
}
pub(super) fn minute_start(unix: u64) -> u64 {
    unix - unix % INTERVAL_SECS
}
pub(super) struct Log {
    pub(super) dir: PathBuf,
    pub(super) devices: BTreeMap<String, DeviceSeries>,
}
#[derive(Default)]
pub(super) struct DeviceSeries {
    pub(super) columns: Vec<String>,
    pub(super) rows: Vec<Row>,
    /// The last row is the minute underway. It stays in memory until it ends.
    pub(super) open: bool,
    /// How many rows, from the first, the file already holds.
    pub(super) written: usize,
    /// The file no longer matches the rows before `written`: a column was
    /// added, or old rows dropped. Written whole next time.
    pub(super) rewrite: bool,
}
pub(super) struct Row {
    pub(super) time: u64,
    pub(super) values: Vec<Option<i32>>,
}
impl Log {
    pub(super) fn open(dir: PathBuf) -> Result<Self> {
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

    pub(super) fn note(&mut self, device: &str, capability: &str, value: f64, unix: u64) {
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

    pub(super) fn samples(&self, device: &str, capability: &str, since: u64) -> Vec<(u64, i32)> {
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

    pub(super) fn forget(&mut self, device: &str) {
        self.devices.remove(device);
        let _ = fs::remove_file(self.file(device));
    }

    pub(super) fn file(&self, device: &str) -> PathBuf {
        self.dir.join(format!("{}.csv", encode_id(device)))
    }

    pub(super) fn match_id(&self, query: &str) -> Result<String> {
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
    pub(super) fn maintenance(&mut self, now: u64, force: bool) {
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
    pub(super) fn write(&mut self) -> Result<()> {
        for (id, device) in &mut self.devices {
            let path = self.dir.join(format!("{}.csv", encode_id(id)));
            device.save(&path)?;
        }
        Ok(())
    }

    pub(super) fn load_file(&mut self, path: &Path) -> Result<()> {
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
    pub(super) fn record(&mut self, capability: &str, milli: i32, unix: u64) {
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

    pub(super) fn column(&mut self, name: &str) -> usize {
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

    pub(super) fn set(&mut self, column: usize, milli: i32) {
        if let Some(row) = self.rows.last_mut() {
            row.values.resize(row.values.len().max(column + 1), None);
            row.values[column] = Some(milli);
        }
    }

    /// Ends the minute underway: kept if it says something new, or the
    /// heartbeat is due, or `force` (the process is stopping).
    pub(super) fn finish(&mut self, force: bool) {
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

    pub(super) fn worth_keeping(&self) -> bool {
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
    pub(super) fn compact(&mut self, now: u64) {
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

    pub(super) fn save(&mut self, path: &Path) -> Result<()> {
        let ended = self.rows.len() - usize::from(self.open);
        if self.rewrite || self.written == 0 {
            if ended == 0 {
                let _ = fs::remove_file(path);
            } else {
                crate::files::write(path, self.csv(&self.rows[..ended]).as_bytes())?;
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

    pub(super) fn ended(&self) -> &[Row] {
        &self.rows[..self.rows.len() - usize::from(self.open)]
    }

    /// Each row's cells by name, in name order, so the column order of a
    /// file does not matter.
    pub(super) fn named(&self, row: &Row) -> BTreeMap<String, i32> {
        self.columns
            .iter()
            .zip(&row.values)
            .filter_map(|(column, value)| Some((column.clone(), (*value)?)))
            .collect()
    }

    pub(super) fn digest(&self) -> BTreeMap<u64, String> {
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

    pub(super) fn export(&self, days: &[u64]) -> Rows {
        self.ended()
            .iter()
            .filter(|row| days.contains(&day(row.time)))
            .map(|row| (row.time, self.named(row)))
            .collect()
    }

    pub(super) fn merge(&mut self, rows: Rows, now: u64) {
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

    pub(super) fn header(&self) -> String {
        let mut out = String::from("time");
        for column in &self.columns {
            out.push(',');
            out.push_str(&escape(column));
        }
        out.push('\n');
        out
    }

    pub(super) fn csv(&self, rows: &[Row]) -> String {
        let mut out = self.header();
        for row in rows {
            out.push_str(&line(row));
        }
        out
    }
}
pub(super) fn day(time: u64) -> u64 {
    time / (24 * 60 * 60)
}
pub(super) fn line(row: &Row) -> String {
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
pub(super) fn append(path: &Path, body: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(body.as_bytes()))
        .with_context(|| format!("could not save {}", path.display()))
}
pub(super) fn encode_id(id: &str) -> String {
    let mut out = String::new();
    for byte in id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
pub(super) fn decode_id(stem: &str) -> Option<String> {
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
pub(super) fn parse_milli(text: &str) -> Option<i32> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    milli_of(text.parse().ok()?)
}
pub(super) fn parse_line(line: &str) -> Vec<String> {
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
pub(super) fn escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}
