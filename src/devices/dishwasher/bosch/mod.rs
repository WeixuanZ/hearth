//! A Bosch dishwasher: one task that holds the Home Connect session and
//! state, and shows it to HomeKit and as capabilities.
//!
//! Home has no dishwasher. The machine is switches. The door, a leak and the
//! end of a cycle are their own accessories, because those are the sensors
//! Home will notify for.

mod capabilities;
mod settings;

pub use settings::BoschDevice;

use crate::{
    connectors::bosch::{
        link,
        protocol::{Command, Reading},
    },
    devices::{
        cycle::{Cycle, Seen},
        runtime,
    },
};
use anyhow::{anyhow, Result};
use capabilities::{Ask, Kept};
use tokio::sync::{mpsc, watch};

/// Bosch dishwashers, over Home Connect on the local network.
pub struct Bosch;

crate::register_driver!(Bosch);

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

    /// The machine with its options, programmes and supplies, and its door,
    /// AquaStop and the end of a cycle as tiles of their own.
    fn homekit_ids(machine: &BoschDevice) -> crate::surfaces::homekit::ids::DeviceIds {
        use crate::connectors::bosch::protocol::{option_switches, programs};
        let mut ids = crate::surfaces::homekit::ids::DeviceIds::default()
            .accessory("main", machine.aid)
            .accessory("door", machine.door_aid)
            .accessory("leak", machine.leak_aid)
            .accessory("cycle", machine.cycle_aid)
            .service("main", "run", 20)
            .service("main", "supply:salt", 700)
            .service("main", "supply:rinse_aid", 720);
        for (key, iid, _) in option_switches(&machine.features) {
            ids = ids.service(
                "main",
                format!("switch:{}", capabilities::option_id(key)),
                iid,
            );
        }
        for (index, (uid, _)) in programs(&machine.features).into_iter().enumerate() {
            ids = ids.service("main", format!("programme:{uid}"), 400 + index as u64 * 10);
        }
        // Each sensor tile counted its iid past its information service.
        ["door", "leak", "cycle"]
            .into_iter()
            .fold(ids, |ids, tile| ids.service(tile, "sensor", 7))
    }
}

async fn spawn(device: BoschDevice, context: crate::driver::Context) -> Result<()> {
    // The programme the next start runs, chosen from any surface.
    let (chosen, choice) = watch::channel(None);
    let shown = device.clone();
    let mut cycle = Cycle::default();
    let states = runtime::published(&context, Reading::default(), move |reading| {
        let kept = keep(&mut cycle, *choice.borrow(), reading);
        capabilities::snapshot(&shown, reading, kept)
    });
    let (commands_tx, mut commands) = mpsc::channel(32);
    answer(
        device.clone(),
        context.requests(),
        states.clone(),
        chosen,
        commands_tx,
    );
    tokio::spawn(async move {
        let offline = |reading: &mut Reading| reading.online = false;
        runtime::keep_connected(&device.name, &states, offline, &mut commands, |commands| {
            let (device, states) = (device.clone(), states.clone());
            Box::pin(async move {
                link::session(&device, Some(&states), Some(commands)).await?;
                Ok(())
            })
        })
        .await;
    });
    Ok(())
}

/// Requests from the menu bar, the CLI and agents: a choice the bridge
/// keeps, shown at once, or a command for the machine, refused while it
/// would be ignored.
fn answer(
    device: BoschDevice,
    requests: mpsc::Receiver<crate::capabilities::Set>,
    states: watch::Sender<Reading>,
    chosen: watch::Sender<Option<u32>>,
    commands: mpsc::Sender<Command>,
) {
    runtime::answer(requests, move |request| {
        let kept = Kept {
            chosen: *chosen.borrow(),
            ..Kept::default()
        };
        let decided = capabilities::command(&device, &states.borrow(), kept, request);
        let (states, chosen, commands) = (states.clone(), chosen.clone(), commands.clone());
        async move {
            match decided? {
                Ask::Send(command) => commands
                    .send(command)
                    .await
                    .map_err(|_| anyhow!("the dishwasher stopped")),
                Ask::Choose(uid) => {
                    chosen.send_replace(Some(uid));
                    // Shown with the machine's state, as one snapshot.
                    states.send_modify(|_| {});
                    Ok(())
                }
            }
        }
    });
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
            let (reports, _) = watch::channel(Reading {
                online: true,
                machine,
            });
            let (chosen_tx, chosen) = watch::channel(None);
            let (requests, requests_rx) = mpsc::channel(4);
            let (commands, sent) = mpsc::channel(4);
            answer(device(), requests_rx, reports.clone(), chosen_tx, commands);
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

    #[tokio::test]
    async fn home_keeps_the_ids_it_knows_the_dishwasher_by() {
        use crate::{driver::Driver, surfaces::homekit::Home};
        use hap::HapType;
        let device = device();
        let snapshot = capabilities::snapshot(&device, &Reading::default(), Kept::default());
        let mut home = Home::of(&snapshot, Bosch::homekit_ids(&device)).await;
        // Start and stop, then the one programme.
        assert_eq!(home.services(2, HapType::Switch), [20, 400]);
        assert_eq!(home.services(3, HapType::ContactSensor), [7]);
        assert_eq!(home.services(4, HapType::LeakSensor), [7]);
        assert_eq!(home.services(5, HapType::MotionSensor), [7]);
        // A programme's tile picks it and starts it, and stops it when off.
        assert_eq!(
            home.write(2, 400, HapType::PowerState, json!(true)).await,
            [("programme", json!("30")), ("run", json!(true))]
        );
        assert_eq!(
            home.write(2, 400, HapType::PowerState, json!(false)).await,
            [("run", json!(false))]
        );
    }
}
