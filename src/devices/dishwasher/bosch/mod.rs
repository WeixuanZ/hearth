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
    devices::cycle::{Cycle, Seen},
    surfaces::homekit::characteristics::{follow_label, follow_name},
};
use accessory::{
    apply_cycle, apply_door, apply_leak, apply_main, build_cycle, build_door, build_leak,
    build_main,
};
use anyhow::{anyhow, Result};
use capabilities::{Ask, Kept};
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
    let snapshot = capabilities::snapshot(&device, &Reading::default(), Kept::default());
    let id = snapshot.id.clone();
    crate::capabilities::publish(snapshot);
    let (requests_tx, mut requests) = mpsc::channel(16);
    crate::capabilities::listen(&id, requests_tx);
    let (state_tx, state_rx) = watch::channel(Reading::default());
    // The programme the next start runs, chosen from the menu bar or CLI.
    let (chosen_tx, chosen_rx) = watch::channel(None);
    let commands = commands_tx.clone();
    let asked = device.clone();
    let known = state_rx.clone();
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            let reading = known.borrow().clone();
            let kept = Kept {
                chosen: *chosen_tx.borrow(),
                ..Kept::default()
            };
            let result = match capabilities::command(&asked, &reading, kept, &request) {
                Ok(Ask::Send(command)) => commands
                    .send(command)
                    .await
                    .map_err(|_| anyhow!("the dishwasher stopped")),
                Ok(Ask::Choose(uid)) => {
                    chosen_tx.send_replace(Some(uid));
                    Ok(())
                }
                Err(error) => Err(error),
            };
            request.finish(result);
        }
    });

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
    let sensor = server
        .add_accessory(build_cycle(&device).await?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    follow_label(&id, sensor.clone(), HapType::MotionSensor, "cycle");

    let shown = device.clone();
    tokio::spawn(async move {
        let updates = async move {
            let mut states = state_rx;
            let mut chosen = chosen_rx;
            let mut cycle = Cycle::default();
            loop {
                let current = states.borrow_and_update().clone();
                // Only a machine whose state has arrived: a session starts
                // empty, which is not the end of a run.
                let machine = &current.machine;
                if current.online && !machine.operation.is_empty() {
                    cycle.see(Seen {
                        running: machine.running(),
                        stopping: machine.stopping(),
                        flagged: machine.finished,
                        opened: machine.door_open,
                    });
                }
                let kept = Kept {
                    chosen: *chosen.borrow_and_update(),
                    finished: cycle.finished(),
                };
                suppress.store(true, Ordering::SeqCst);
                let result = async {
                    apply_main(main.lock().await.as_mut(), &shown, &current).await?;
                    apply_door(door.lock().await.as_mut(), &current).await?;
                    apply_leak(leak.lock().await.as_mut(), &current).await?;
                    apply_cycle(sensor.lock().await.as_mut(), &current, kept.finished).await?;
                    Ok::<(), anyhow::Error>(())
                }
                .await;
                suppress.store(false, Ordering::SeqCst);
                if let Err(error) = result {
                    log::warn!("dishwasher HomeKit update failed: {error:#}");
                }
                crate::capabilities::publish(capabilities::snapshot(&shown, &current, kept));
                let changed = tokio::select! {
                    changed = states.changed() => changed,
                    changed = chosen.changed() => changed,
                };
                if changed.is_err() {
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
        // Asked for while it was unreachable, as from Home: dropped, not
        // sent late. A session takes commands only once it is connected.
        while let Ok(command) = commands.try_recv() {
            log::info!(
                "{}: not sent, it was not connected: {command:?}",
                device.name
            );
        }
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
