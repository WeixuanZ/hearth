//! The time, as hearth stamps and compares it: since 1970, in UTC.

use std::time::{SystemTime, UNIX_EPOCH};

/// Now, in seconds.
pub fn now() -> u64 {
    now_ms() / 1000
}

/// Now, in milliseconds, for what is timed closer, like the group's hellos.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}
