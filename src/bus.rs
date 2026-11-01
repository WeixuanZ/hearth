//! The devices as one watched value: every device's latest snapshot.
//!
//! A device publishes its snapshot (see `capabilities::publish`), which puts
//! it here. Whoever wants to know watches: they see the current state at
//! once, then each new one. A slow watcher skips to the latest state rather
//! than missing any, so nothing is lost, only coalesced. Everything hearth
//! tracks is state, a finished cycle and an alert included, so there are no
//! other events. A watcher that needs what changed, like a stream of
//! changes on the socket, diffs two states with [`changes`]; that is also
//! where topics are named, `hearth/<device>/<capability>`, as MQTT would.

use crate::capabilities::{Alert, Snapshot};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock},
};
use tokio::sync::watch;

/// Every device by id. Shared, so each state costs only what changed.
pub type Devices = BTreeMap<String, Arc<Snapshot>>;

static STATE: LazyLock<watch::Sender<Arc<Devices>>> =
    LazyLock::new(|| watch::Sender::new(Arc::default()));

pub fn watch() -> watch::Receiver<Arc<Devices>> {
    STATE.subscribe()
}

/// Puts `snapshot` in, telling watchers only if it differs.
pub fn put(snapshot: Snapshot) {
    STATE.send_if_modified(|devices| {
        if devices
            .get(&snapshot.id)
            .is_some_and(|held| **held == snapshot)
        {
            return false;
        }
        Arc::make_mut(devices).insert(snapshot.id.clone(), Arc::new(snapshot));
        true
    });
}

/// Empties it, when this computer stops serving Home.
pub fn clear() {
    STATE.send_if_modified(|devices| {
        let had = !devices.is_empty();
        *devices = Arc::default();
        had
    });
}

/// One difference between two states, as the socket streams it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Change {
    /// A device as it now is: new, or changed in any way.
    Device {
        topic: String,
        device: Arc<Snapshot>,
    },
    /// One capability's new value.
    Changed {
        topic: String,
        device: String,
        capability: &'static str,
        value: Value,
    },
    Raised {
        topic: String,
        device: String,
        alert: Alert,
    },
    Cleared {
        topic: String,
        device: String,
        alert: &'static str,
    },
    Removed {
        topic: String,
        device: String,
    },
}

impl Change {
    pub fn topic(&self) -> &str {
        match self {
            Self::Device { topic, .. }
            | Self::Changed { topic, .. }
            | Self::Raised { topic, .. }
            | Self::Cleared { topic, .. }
            | Self::Removed { topic, .. } => topic,
        }
    }
}

fn topic(device: &str, part: &str) -> String {
    format!("hearth/{device}/{part}")
}

/// What changed from `before` to `after`. Devices not in `before` are new:
/// all of them, then, from an empty state.
pub fn changes(before: &Devices, after: &Devices) -> Vec<Change> {
    let mut out = Vec::new();
    for (id, now) in after {
        let was = before.get(id);
        if was.is_some_and(|was| Arc::ptr_eq(was, now) || was == now) {
            continue;
        }
        device_changes(was.map(Arc::as_ref), now, &mut out);
    }
    out.extend(
        before
            .keys()
            .filter(|id| !after.contains_key(*id))
            .map(|id| Change::Removed {
                topic: topic(id, "device"),
                device: id.clone(),
            }),
    );
    out
}

fn device_changes(before: Option<&Snapshot>, after: &Arc<Snapshot>, out: &mut Vec<Change>) {
    let id = &after.id;
    out.push(Change::Device {
        topic: topic(id, "device"),
        device: after.clone(),
    });
    let was = |capability: &str| {
        before.and_then(|snapshot| {
            snapshot
                .capabilities
                .iter()
                .find(|item| item.id == capability)
                .map(|item| &item.value)
        })
    };
    out.extend(
        after
            .capabilities
            .iter()
            .filter(|item| was(item.id) != Some(&item.value))
            .map(|item| Change::Changed {
                topic: topic(id, item.id),
                device: id.clone(),
                capability: item.id,
                value: item.value.clone(),
            }),
    );
    let held = |snapshot: Option<&Snapshot>, alert: &str| {
        snapshot.is_some_and(|snapshot| snapshot.alerts.iter().any(|held| held.id == alert))
    };
    out.extend(
        after
            .alerts
            .iter()
            .filter(|alert| !held(before, alert.id))
            .map(|alert| Change::Raised {
                topic: topic(id, &format!("alert/{}", alert.id)),
                device: id.clone(),
                alert: alert.clone(),
            }),
    );
    out.extend(
        before
            .into_iter()
            .flat_map(|snapshot| &snapshot.alerts)
            .filter(|alert| !held(Some(after), alert.id))
            .map(|alert| Change::Cleared {
                topic: topic(id, &format!("alert/{}", alert.id)),
                device: id.clone(),
                alert: alert.id,
            }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::Capability;

    fn snapshot(on: bool, alerts: Vec<Alert>) -> Arc<Snapshot> {
        Arc::new(Snapshot {
            id: "abc:1".into(),
            kind: "abc",
            category: "purifier",
            name: "Fan".into(),
            online: true,
            headline: String::new(),
            detail: String::new(),
            capabilities: vec![
                Capability::power(on),
                Capability::reading("pm25", "PM2.5", "µg/m³", Some(4.0)),
            ],
            alerts,
        })
    }

    fn state(snapshot: &Arc<Snapshot>) -> Devices {
        Devices::from([(snapshot.id.clone(), snapshot.clone())])
    }

    fn topics(changes: &[Change]) -> Vec<&str> {
        changes.iter().map(Change::topic).collect()
    }

    #[test]
    fn from_nothing_every_device_is_new_and_a_repeat_is_nothing() {
        let first = state(&snapshot(false, Vec::new()));
        assert_eq!(
            topics(&changes(&Devices::new(), &first)),
            [
                "hearth/abc:1/device",
                "hearth/abc:1/power",
                "hearth/abc:1/pm25"
            ]
        );
        assert!(changes(&first, &first).is_empty());
        let same = state(&snapshot(false, Vec::new()));
        assert!(
            changes(&first, &same).is_empty(),
            "equal, though not the same allocation"
        );
    }

    #[test]
    fn only_what_changed_and_alerts_come_and_go_and_devices_leave() {
        let off = state(&snapshot(false, Vec::new()));
        let on = state(&snapshot(
            true,
            vec![Alert::new("filter", "Replace the filter.")],
        ));
        let found = changes(&off, &on);
        assert_eq!(
            topics(&found),
            [
                "hearth/abc:1/device",
                "hearth/abc:1/power",
                "hearth/abc:1/alert/filter"
            ]
        );
        assert!(
            matches!(&found[1], Change::Changed { value, .. } if *value == serde_json::json!(true))
        );
        assert!(matches!(
            changes(&on, &off).last(),
            Some(Change::Cleared {
                alert: "filter",
                ..
            })
        ));
        assert!(matches!(
            changes(&off, &Devices::new()).as_slice(),
            [Change::Removed { .. }]
        ));
    }
}
