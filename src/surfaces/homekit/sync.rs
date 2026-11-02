//! Keeps a device's accessories in step with it: each new state is shown in
//! Home, and what Home writes is asked of the device as capability sets, the
//! same requests the menu bar, the CLI and agents make.

use super::{
    accessory::{show, Asks},
    characteristics::{follow_label, follow_name},
    profile::Made,
};
use crate::{bus, capabilities};
use anyhow::{anyhow, Result};
use hap::{
    accessory::HapAccessory,
    futures::lock::Mutex,
    server::{IpServer, Server},
};
use std::{
    collections::BTreeMap,
    sync::{atomic::Ordering, Arc},
};
use tokio::sync::mpsc;

/// Adds `made` to the bridge for `device`, and keeps it in step.
pub async fn add(
    server: &IpServer,
    device: &str,
    made: Made,
    mut writes: mpsc::Receiver<Asks>,
) -> Result<()> {
    let Made {
        accessories,
        wiring,
        names,
    } = made;
    let mut added: BTreeMap<u64, Arc<Mutex<Box<dyn HapAccessory>>>> = BTreeMap::new();
    for accessory in accessories {
        let id = accessory.id;
        let pointer = server
            .add_accessory(accessory)
            .await
            .map_err(|error| anyhow!("{error}"))?;
        added.insert(id, pointer);
    }
    // Renames reach Home, and come back from it.
    for (aid, service, suffix) in names {
        let Some(accessory) = added.get(&aid).cloned() else {
            continue;
        };
        match suffix {
            None => follow_name(device, accessory, service),
            Some(suffix) => follow_label(device, accessory, service, suffix),
        }
    }

    let id = device.to_owned();
    tokio::spawn(async move {
        while let Some(asks) = writes.recv().await {
            for (capability, value) in asks {
                log::info!("{id}: Home asks {capability} = {value}");
                if let Err(error) = capabilities::set(&id, capability, value).await {
                    log::info!("{id}: {error:#}");
                    break;
                }
            }
        }
    });

    let id = device.to_owned();
    let mut state = bus::watch();
    tokio::spawn(async move {
        let mut shown: Option<Arc<capabilities::Snapshot>> = None;
        loop {
            let current = state.borrow_and_update().get(&id).cloned();
            if let Some(snapshot) =
                current.filter(|now| !shown.as_ref().is_some_and(|was| Arc::ptr_eq(was, now)))
            {
                wiring.seen(snapshot.clone());
                wiring.quiet.store(true, Ordering::SeqCst);
                for binding in &wiring.bindings {
                    let (Some(accessory), Some(value)) =
                        (added.get(&binding.aid), (binding.read)(&snapshot))
                    else {
                        continue;
                    };
                    if let Err(error) =
                        show(accessory.lock().await.as_mut(), binding.iid, value).await
                    {
                        log::debug!("{id}: Home update failed: {error:#}");
                    }
                }
                wiring.quiet.store(false, Ordering::SeqCst);
                shown = Some(snapshot);
            }
            if state.changed().await.is_err() {
                break;
            }
        }
    });
    Ok(())
}
