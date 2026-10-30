//! A Bosch dishwasher: one task that holds the Home Connect session and
//! state, and shows it to HomeKit and as capabilities.
//!
//! Home has no dishwasher. The machine is switches. The door, a leak and the
//! end of a cycle are their own accessories, because those are the sensors
//! Home will notify for.

mod accessory;
mod capabilities;

use crate::{
    config::BoschDevice,
    connectors::bosch::{
        link,
        protocol::{Command, Reading},
    },
    surfaces::homekit::characteristics::{follow_label, follow_name},
};
use accessory::{
    apply_cycle, apply_door, apply_leak, apply_main, build_cycle, build_door, build_leak,
    build_main,
};
use anyhow::{anyhow, Result};
use hap::{
    server::{IpServer, Server},
    HapType,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

pub async fn spawn(device: BoschDevice, server: IpServer) -> Result<()> {
    let (commands_tx, commands_rx) = mpsc::channel(32);
    let snapshot = capabilities::snapshot(&device, &Reading::default());
    let id = snapshot.id.clone();
    crate::capabilities::publish(snapshot);
    let (requests_tx, mut requests) = mpsc::channel(16);
    crate::capabilities::listen(&id, requests_tx);
    let commands = commands_tx.clone();
    let asked = device.clone();
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            let result = match capabilities::command(&asked, &request) {
                Ok(command) => commands
                    .send(command)
                    .await
                    .map_err(|_| anyhow!("the dishwasher stopped")),
                Err(error) => Err(error),
            };
            request.finish(result);
        }
    });

    let (state_tx, state_rx) = watch::channel(Reading::default());
    let suppress = Arc::new(AtomicBool::new(false));
    let main = server
        .add_accessory(build_main(&device, &suppress, &commands_tx).await?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    follow_name(&id, main.clone(), HapType::Switch);
    let door = server
        .add_accessory(build_door(&device).await?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    follow_label(&id, door.clone(), HapType::ContactSensor, "door");
    let leak = server
        .add_accessory(build_leak(&device).await?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    follow_label(&id, leak.clone(), HapType::LeakSensor, "AquaStop");
    let cycle = server
        .add_accessory(build_cycle(&device).await?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    follow_label(&id, cycle.clone(), HapType::MotionSensor, "cycle");

    let shown = device.clone();
    tokio::spawn(async move {
        let updates = async move {
            let mut states = state_rx;
            let mut saw_unfinished = false;
            let mut motion = false;
            loop {
                let current = states.borrow().clone();
                if current.online && !current.machine.finished {
                    saw_unfinished = true;
                }
                if current.online {
                    motion = saw_unfinished && current.machine.finished;
                }
                suppress.store(true, Ordering::SeqCst);
                let result = async {
                    apply_main(main.lock().await.as_mut(), &shown, &current).await?;
                    apply_door(door.lock().await.as_mut(), &current).await?;
                    apply_leak(leak.lock().await.as_mut(), &current).await?;
                    apply_cycle(cycle.lock().await.as_mut(), &current, motion).await?;
                    Ok::<(), anyhow::Error>(())
                }
                .await;
                suppress.store(false, Ordering::SeqCst);
                if let Err(error) = result {
                    log::warn!("dishwasher HomeKit update failed: {error:#}");
                }
                crate::capabilities::publish(capabilities::snapshot(&shown, &current));
                if states.changed().await.is_err() {
                    break;
                }
            }
        };
        tokio::pin!(updates);
        tokio::select! {
            result = drive(device, state_tx, commands_rx) => {
                if let Err(error) = result {
                    log::error!("dishwasher stopped: {error:#}");
                }
            }
            () = &mut updates => {}
        }
    });
    Ok(())
}

async fn drive(
    device: BoschDevice,
    states: watch::Sender<Reading>,
    mut commands: mpsc::Receiver<Command>,
) -> Result<()> {
    let mut delay = 2u64;
    loop {
        match link::session(&device, Some(&states), Some(&mut commands)).await {
            Ok(_) => {
                log::info!("{} connection closed", device.name);
                // It was up, so the next try is a fresh one, not a backoff.
                delay = 2;
            }
            Err(error) => log::warn!("{}: {error:#}", device.name),
        }
        if commands.is_closed() {
            break;
        }
        states.send_modify(|reading| reading.online = false);
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
    Ok(())
}
