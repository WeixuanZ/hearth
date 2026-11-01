//! A Bosch dishwasher: one task that holds the Home Connect session and
//! state, and shows it to HomeKit and as capabilities.
//!
//! Home has no dishwasher. The machine is switches. The door, a leak and the
//! end of a cycle are their own accessories, because those are the sensors
//! Home will notify for.

mod accessory;
mod capabilities;
mod settings;

pub use settings::BoschDevice;

use crate::{
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
use hap::{server::Server, HapType};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

/// Bosch dishwashers, over Home Connect on the local network.
pub struct Bosch;

impl crate::driver::Driver for Bosch {
    const KIND: &'static str = "bosch";
    const NAME: &'static str = "Bosch dishwasher";
    const CATEGORY: &'static str = "dishwasher";
    type Settings = BoschDevice;
    type Setup = crate::connectors::bosch::Setup;

    fn setup() -> (Self::Setup, crate::setup::Step) {
        Self::Setup::start()
    }

    async fn start(settings: BoschDevice, context: crate::driver::Context) -> Result<()> {
        spawn(settings, context).await
    }
}

async fn spawn(device: BoschDevice, context: crate::driver::Context) -> Result<()> {
    let server = context.homekit().clone();
    let (commands_tx, commands_rx) = mpsc::channel(32);
    let snapshot = capabilities::snapshot(&device, &Reading::default(), Kept::default());
    let id = snapshot.id.clone();
    context.publish(snapshot);
    let requests = context.requests();
    let (state_tx, state_rx) = watch::channel(Reading::default());
    // The programme the next start runs, chosen from the menu bar or CLI.
    let (chosen_tx, chosen_rx) = watch::channel(None);
    tokio::spawn(answer(
        device.clone(),
        requests,
        state_rx.clone(),
        chosen_tx,
        commands_tx.clone(),
    ));

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
                let kept = keep(&mut cycle, *chosen.borrow_and_update(), &current);
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

/// Requests from the menu bar, the CLI and agents: a choice the bridge
/// keeps, or a command for the machine, refused while it would be ignored.
async fn answer(
    device: BoschDevice,
    mut requests: mpsc::Receiver<crate::capabilities::Set>,
    known: watch::Receiver<Reading>,
    chosen: watch::Sender<Option<u32>>,
    commands: mpsc::Sender<Command>,
) {
    while let Some(request) = requests.recv().await {
        let reading = known.borrow().clone();
        let kept = Kept {
            chosen: *chosen.borrow(),
            ..Kept::default()
        };
        let result = match capabilities::command(&device, &reading, kept, &request) {
            Ok(Ask::Send(command)) => commands
                .send(command)
                .await
                .map_err(|_| anyhow!("the dishwasher stopped")),
            Ok(Ask::Choose(uid)) => {
                chosen.send_replace(Some(uid));
                Ok(())
            }
            Err(error) => Err(error),
        };
        request.finish(result);
    }
}

/// What the bridge keeps as of this reading. A machine whose state has not
/// arrived is not looked at: a session starts empty, which is not the end
/// of a run.
fn keep(cycle: &mut Cycle, chosen: Option<u32>, reading: &Reading) -> Kept {
    let machine = &reading.machine;
    if reading.online && !machine.operation.is_empty() {
        cycle.see(Seen {
            running: machine.running(),
            stopping: machine.stopping(),
            flagged: machine.finished,
            opened: machine.door_open,
        });
    }
    Kept {
        chosen,
        finished: cycle.finished(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capabilities::Set, connectors::bosch::protocol::BoschFeature,
        connectors::bosch::protocol::Machine,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    const RUN: &str = "BSH.Common.EnumType.OperationState.Run";
    const READY: &str = "BSH.Common.EnumType.OperationState.Ready";

    fn device() -> BoschDevice {
        let feature = |name: &str| BoschFeature {
            name: name.into(),
            values: BTreeMap::new(),
        };
        BoschDevice {
            aid: 2,
            door_aid: 3,
            leak_aid: 4,
            cycle_aid: 5,
            name: "Dishwasher".into(),
            brand: "Bosch".into(),
            model: "XY01".into(),
            ha_id: "BOSCH-HA".into(),
            host: "dishwasher".into(),
            key: "secret".into(),
            iv: String::new(),
            features: BTreeMap::from([
                ("30".into(), feature("Dishcare.Dishwasher.Program.Eco50")),
                ("40".into(), feature("BSH.Common.Command.AbortProgram")),
            ]),
        }
    }

    /// The machine's side: the state it reports, and what it was sent.
    struct Fake {
        reports: watch::Sender<Reading>,
        sent: mpsc::Receiver<Command>,
        requests: mpsc::Sender<Set>,
        chosen: watch::Receiver<Option<u32>>,
    }

    impl Fake {
        fn start(machine: Machine) -> Self {
            let (reports, known) = watch::channel(Reading {
                online: true,
                machine,
            });
            let (chosen_tx, chosen) = watch::channel(None);
            let (requests, requests_rx) = mpsc::channel(4);
            let (commands, sent) = mpsc::channel(4);
            tokio::spawn(answer(device(), requests_rx, known, chosen_tx, commands));
            Self {
                reports,
                sent,
                requests,
                chosen,
            }
        }

        async fn ask(&self, capability: &str, value: serde_json::Value) -> Result<()> {
            let (request, answer) = Set::asked(capability, value);
            self.requests.send(request).await.unwrap();
            answer.await.unwrap()
        }

        fn nothing_sent(&mut self) -> bool {
            self.sent.try_recv().is_err()
        }
    }

    #[tokio::test]
    async fn choosing_sends_nothing_and_start_sends_the_choice() {
        let mut fake = Fake::start(Machine {
            operation: READY.into(),
            remote_start: true,
            ..Machine::default()
        });
        fake.ask("programme", json!("Eco 50")).await.unwrap();
        assert!(fake.nothing_sent());
        assert_eq!(*fake.chosen.borrow(), Some(30));

        fake.ask("run", json!(true)).await.unwrap();
        assert_eq!(fake.sent.recv().await, Some(Command::Program(30)));
    }

    #[tokio::test]
    async fn a_machine_that_is_not_connected_or_not_allowing_remote_start_is_sent_nothing() {
        let mut fake = Fake::start(Machine {
            operation: READY.into(),
            ..Machine::default()
        });
        fake.ask("programme", json!("30")).await.unwrap();
        let refused = fake.ask("run", json!(true)).await.unwrap_err().to_string();
        assert!(refused.contains("Remote Start"));
        fake.reports.send_modify(|reading| {
            reading.online = false;
            reading.machine.remote_start = true;
        });
        let refused = fake.ask("run", json!(true)).await.unwrap_err().to_string();
        assert!(refused.contains("not connected"));
        assert!(fake.nothing_sent());
    }

    #[tokio::test]
    async fn a_running_programme_is_stopped() {
        let mut fake = Fake::start(Machine {
            operation: RUN.into(),
            ..Machine::default()
        });
        fake.ask("run", json!(false)).await.unwrap();
        assert_eq!(fake.sent.recv().await, Some(Command::Abort));
    }

    #[test]
    fn a_run_seen_to_end_has_finished_but_a_reconnect_has_not() {
        let mut cycle = Cycle::default();
        let reading = |online, operation: &str| Reading {
            online,
            machine: Machine {
                operation: operation.into(),
                ..Machine::default()
            },
        };
        assert!(!keep(&mut cycle, None, &reading(true, RUN)).finished);
        // A new session starts with an empty machine: not the end of the run.
        assert!(!keep(&mut cycle, None, &reading(true, "")).finished);
        assert!(!keep(&mut cycle, None, &reading(false, READY)).finished);
        assert!(keep(&mut cycle, Some(30), &reading(true, READY)).finished);
        assert_eq!(
            keep(&mut cycle, Some(30), &reading(true, READY)).chosen,
            Some(30)
        );
    }
}
