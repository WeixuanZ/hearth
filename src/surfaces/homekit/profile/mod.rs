//! Accessories made from a device's capabilities. Nothing here knows a
//! brand: what a device is (its category) picks the main service, and what
//! each capability is (its role) picks where it goes and how its value
//! reads. A device of a new kind shows in Home with no code here.
//!
//! Every service takes its id from [`Ids`], by what it is, so Home keeps the
//! same ids, and with them rooms, scenes and automations, whatever changes.

use super::{
    accessory::{fixed, Accessory, Asks, Wiring},
    characteristics::{configured_name, device_name},
    ids::Ids,
};
use crate::capabilities::{Capability, Kind, Role, Snapshot};
use anyhow::{anyhow, Result};
use hap::{
    accessory::AccessoryInformation,
    characteristic::HapCharacteristic,
    service::{
        accessory_information::AccessoryInformationService, air_purifier::AirPurifierService,
        air_quality_sensor::AirQualitySensorService, contact_sensor::ContactSensorService,
        filter_maintenance::FilterMaintenanceService, heater_cooler::HeaterCoolerService,
        humidity_sensor::HumiditySensorService, input_source::InputSourceService,
        leak_sensor::LeakSensorService, lightbulb::LightbulbService,
        motion_sensor::MotionSensorService, speaker::SpeakerService, switch::SwitchService,
        television::TelevisionService, temperature_sensor::TemperatureSensorService, HapService,
    },
    HapType,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;

mod light;
mod machine;
mod purifier;
mod sensors;
mod tv;

/// A device's accessories, and how to keep them in step with it.
pub struct Made {
    pub accessories: Vec<Accessory>,
    pub wiring: Wiring,
    /// Each accessory's main service, and what its name adds to the
    /// device's: none for the device's own tile, `door` for "Dishwasher door".
    pub names: Vec<(u64, HapType, Option<String>)>,
}

pub async fn make(snapshot: &Snapshot, ids: &mut Ids, writes: mpsc::Sender<Asks>) -> Result<Made> {
    let mut maker = Maker {
        device: snapshot.clone(),
        ids,
        wiring: Wiring::new(writes, snapshot),
        accessories: Vec::new(),
        names: Vec::new(),
    };
    match snapshot.category {
        "tv" => maker.tv().await?,
        "purifier" => maker.purifier().await?,
        "light" => maker.light().await?,
        "sensor" => maker.sensor().await?,
        _ => maker.machine().await?,
    }
    Ok(Made {
        accessories: maker.accessories,
        wiring: maker.wiring,
        names: maker.names,
    })
}

struct Maker<'a> {
    device: Snapshot,
    ids: &'a mut Ids,
    wiring: Wiring,
    accessories: Vec<Accessory>,
    names: Vec<(u64, HapType, Option<String>)>,
}

/// A capability's current value.
fn value(snapshot: &Snapshot, id: &str) -> Option<Value> {
    snapshot
        .capabilities
        .iter()
        .find(|capability| capability.id == id)
        .map(|capability| capability.value.clone())
}

fn on(snapshot: &Snapshot, id: &str) -> bool {
    value(snapshot, id).and_then(|value| value.as_bool()) == Some(true)
}

fn number(snapshot: &Snapshot, id: &str) -> Option<f64> {
    value(snapshot, id).and_then(|value| value.as_f64())
}

/// Whether a capability is giving a value now: a sensor that is not
/// measuring shows as inactive.
fn active(snapshot: &Snapshot, id: &str) -> bool {
    snapshot
        .capabilities
        .iter()
        .any(|capability| capability.id == id && capability.active && !capability.value.is_null())
}

fn flag(on: bool) -> Value {
    json!(u8::from(on))
}

impl Maker<'_> {
    fn id(&self) -> String {
        self.device.id.clone()
    }

    fn role(&self, role: Role) -> Option<Capability> {
        self.device
            .capabilities
            .iter()
            .find(|capability| capability.role == role)
            .cloned()
    }

    fn aid(&mut self, accessory: &str) -> u64 {
        let id = self.id();
        self.ids.aid(&id, accessory)
    }

    fn iid(&mut self, accessory: &str, service: &str) -> u64 {
        let id = self.id();
        self.ids.iid(&id, accessory, service)
    }

    /// The information service, for the device's own tile or one of its own
    /// beside it (`suffix`), which takes the device's serial and the suffix.
    fn information(
        &self,
        aid: u64,
        name: &str,
        suffix: &str,
        firmware: bool,
    ) -> Result<AccessoryInformationService> {
        self.information_named(aid, name, suffix, firmware, false)
    }

    /// As `information`; `named` also gives it a Configured Name, as a
    /// machine's tile has always had.
    fn information_named(
        &self,
        aid: u64,
        name: &str,
        suffix: &str,
        firmware: bool,
        named: bool,
    ) -> Result<AccessoryInformationService> {
        let about = &self.device.about;
        let serial = if suffix.is_empty() {
            about.serial.clone()
        } else {
            format!("{}-{suffix}", about.serial)
        };
        AccessoryInformation {
            name: name.into(),
            manufacturer: about.maker.clone(),
            model: about.model.clone(),
            serial_number: serial,
            firmware_revision: (firmware && !about.firmware.is_empty())
                .then(|| about.firmware.clone()),
            configured_name: named.then(|| name.to_owned()),
            ..Default::default()
        }
        .to_service(1, aid)
        .map_err(|error| anyhow!("{error}"))
    }

    /// The device's own tile, named after it; a rename in Home renames it.
    async fn name_main(
        &self,
        name: Option<&mut impl HapCharacteristic>,
        configured: &mut (impl HapCharacteristic + hap::characteristic::CharacteristicCallbacks<String>),
    ) -> Result<()> {
        fixed(name, json!(self.device.name)).await?;
        device_name(configured, self.id(), &self.device.name).await
    }

    fn accessory(
        &mut self,
        id: u64,
        services: Vec<Box<dyn HapService>>,
        main: HapType,
        suffix: Option<String>,
    ) {
        self.names.push((id, main, suffix));
        self.accessories.push(Accessory { id, services });
    }

    /// A switch for a capability that is on or off.
    async fn switch(
        &mut self,
        accessory: &str,
        aid: u64,
        key: &str,
        label: &str,
        capability: &'static str,
    ) -> Result<SwitchService> {
        let iid = self.iid(accessory, key);
        let mut switch = SwitchService::new(iid, aid);
        fixed(switch.name.as_mut(), json!(label)).await?;
        if let Some(name) = switch.configured_name.as_mut() {
            configured_name(name, aid, label).await?;
        }
        self.wiring.read(aid, &switch.power_state, move |snapshot| {
            Some(json!(on(snapshot, capability)))
        });
        self.wiring.write(&mut switch.power_state, move |on| {
            vec![(capability, json!(on))]
        });
        Ok(switch)
    }

    /// One switch per option of a choice Home has no picker for: on picks it.
    async fn option_switches(
        &mut self,
        aid: u64,
        choice: &Capability,
        prefix: &str,
        then: Asks,
    ) -> Result<Vec<SwitchService>> {
        let Kind::Choice { options } = &choice.kind else {
            return Ok(Vec::new());
        };
        let mut switches = Vec::new();
        for option in options {
            let key = format!("{prefix}:{}", option.id);
            let label = if prefix == "programme" {
                option.name.clone()
            } else {
                format!("{} {}", choice.name, option.name)
            };
            let iid = self.iid("main", &key);
            let mut switch = SwitchService::new(iid, aid);
            fixed(switch.name.as_mut(), json!(label)).await?;
            if let Some(name) = switch.configured_name.as_mut() {
                configured_name(name, aid, &label).await?;
            }
            let (id, chosen) = (choice.id, option.id.clone());
            let shown = chosen.clone();
            let running = then.iter().any(|(id, _)| *id == "run");
            self.wiring.read(aid, &switch.power_state, move |snapshot| {
                let picked = value(snapshot, id)
                    .and_then(|value| value.as_str().map(str::to_owned))
                    == Some(shown.clone());
                Some(json!(picked && (!running || on(snapshot, "run"))))
            });
            let then = then.clone();
            self.wiring.write(&mut switch.power_state, move |on| {
                if !on {
                    // Off ends a programme it started; off on a picker asks nothing.
                    return if running {
                        vec![("run", json!(false))]
                    } else {
                        Vec::new()
                    };
                }
                let mut asks = vec![(id, json!(chosen.clone()))];
                asks.extend(then.clone());
                asks
            });
            switches.push(switch);
        }
        Ok(switches)
    }

    /// The switches Home shows inside the device's tile: plain switches, and
    /// pickers as one switch per option.
    async fn linked_switches(
        &mut self,
        aid: u64,
        skip: &[Role],
    ) -> Result<Vec<Box<dyn HapService>>> {
        let mut services: Vec<Box<dyn HapService>> = Vec::new();
        for capability in self.device.capabilities.clone() {
            if skip.contains(&capability.role) {
                continue;
            }
            match capability.kind {
                Kind::Switch => {
                    let switch = self
                        .switch(
                            "main",
                            aid,
                            &format!("switch:{}", capability.id),
                            &capability.name,
                            capability.id,
                        )
                        .await?;
                    services.push(Box::new(switch));
                }
                Kind::Choice { .. } if capability.role == Role::Generic => {
                    for switch in self
                        .option_switches(
                            aid,
                            &capability,
                            &format!("option:{}", capability.id),
                            Vec::new(),
                        )
                        .await?
                    {
                        services.push(Box::new(switch));
                    }
                }
                _ => {}
            }
        }
        Ok(services)
    }
}

/// Narrows a number characteristic to what the device accepts.
fn limit(
    characteristic: &mut impl HapCharacteristic,
    min: Option<f32>,
    max: Option<f32>,
    step: Option<f32>,
) -> Result<()> {
    let value = |value: Option<f32>| value.map(|value| json!(value));
    if min.is_some() {
        HapCharacteristic::set_min_value(characteristic, value(min))
            .map_err(|error| anyhow!("{error}"))?;
    }
    if max.is_some() {
        HapCharacteristic::set_max_value(characteristic, value(max))
            .map_err(|error| anyhow!("{error}"))?;
    }
    if step.is_some() {
        HapCharacteristic::set_step_value(characteristic, value(step))
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}
