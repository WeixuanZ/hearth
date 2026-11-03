//! What is asked of the readings: a window of them, a reading's usual
//! day, and how a value is written.

use super::*;

/// The hours asked for, clamped to the retention window, and the Unix second
/// to read from.
pub fn window(hours: u64, now: u64) -> (u64, u64) {
    let hours = hours.clamp(1, RETAIN_SECS / 3600);
    let since = now.saturating_sub(hours.saturating_mul(3600));
    let earliest = now.saturating_sub(RETAIN_SECS);
    (hours, since.max(earliest))
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
pub(super) fn percentile(sorted: &[f64], share: f64) -> f64 {
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
pub(super) fn series_json(series: &SeriesOut<'_>) -> Value {
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
pub(super) fn json_number(milli: i32) -> Value {
    serde_json::from_str(&format_value(milli)).unwrap_or(Value::Null)
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
