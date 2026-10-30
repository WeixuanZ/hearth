//! The dishwasher in Home.
//!
//! Home has no dishwasher. The machine is one switch accessory: power, then a
//! switch for each programme and option the machine lists. The door, a leak
//! and the end of a cycle are their own accessories. Those are the sensors
//! Home will actually notify for.

use crate::connectors::bosch::protocol::{
    label_of, option_switches, programs, salt_name, Command, Reading,
};
use crate::{
    config::{BoschDevice, Settings as _},
    surfaces::homekit::characteristics::{configured_name, device_name, hook_bool, put},
};
use anyhow::{anyhow, Result};
use hap::{
    accessory::{
        contact_sensor::ContactSensorAccessory, leak_sensor::LeakSensorAccessory,
        motion_sensor::MotionSensorAccessory, AccessoryInformation, HapAccessory,
    },
    characteristic::HapCharacteristic,
    service::{
        accessory_information::AccessoryInformationService,
        filter_maintenance::FilterMaintenanceService, switch::SwitchService, HapService,
    },
    HapType,
};
use serde::ser::{Serialize, SerializeStruct, Serializer};
use std::sync::{atomic::AtomicBool, Arc};
use tokio::sync::mpsc;

const POWER: u64 = 20;
const PROGRAM: u64 = 400;
const SALT: u64 = 700;
const RINSE: u64 = 720;

pub(super) struct DishwasherAccessory {
    id: u64,
    info: AccessoryInformationService,
    power: SwitchService,
    switches: Vec<SwitchService>,
    salt: Option<FilterMaintenanceService>,
    rinse: Option<FilterMaintenanceService>,
}

impl HapAccessory for DishwasherAccessory {
    fn get_id(&self) -> u64 {
        self.id
    }
    fn set_id(&mut self, id: u64) {
        self.id = id;
    }
    fn get_service(&self, hap_type: HapType) -> Option<&dyn HapService> {
        self.get_services()
            .into_iter()
            .find(|service| service.get_type() == hap_type)
    }
    fn get_mut_service(&mut self, hap_type: HapType) -> Option<&mut dyn HapService> {
        self.get_mut_services()
            .into_iter()
            .find(|service| service.get_type() == hap_type)
    }
    fn get_services(&self) -> Vec<&dyn HapService> {
        let mut services: Vec<&dyn HapService> = vec![&self.info, &self.power];
        services.extend(self.switches.iter().map(|switch| switch as &dyn HapService));
        if let Some(salt) = &self.salt {
            services.push(salt);
        }
        if let Some(rinse) = &self.rinse {
            services.push(rinse);
        }
        services
    }
    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        let mut services: Vec<&mut dyn HapService> = vec![&mut self.info, &mut self.power];
        services.extend(
            self.switches
                .iter_mut()
                .map(|switch| switch as &mut dyn HapService),
        );
        if let Some(salt) = &mut self.salt {
            services.push(salt);
        }
        if let Some(rinse) = &mut self.rinse {
            services.push(rinse);
        }
        services
    }
}

impl Serialize for DishwasherAccessory {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("HapAccessory", 2)?;
        state.serialize_field("aid", &self.get_id())?;
        state.serialize_field("services", &self.get_services())?;
        state.end()
    }
}

pub(super) async fn build_main(
    device: &BoschDevice,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Command>,
) -> Result<DishwasherAccessory> {
    let aid = device.aid;
    let mut linked = Vec::new();
    let mut switches = Vec::new();
    for (key, iid, label) in option_switches(&device.features) {
        let key = key.to_owned();
        switches.push(
            switch(iid, aid, label, suppress, tx, move |on| Command::Option {
                key: key.clone(),
                on,
            })
            .await?,
        );
        linked.push(iid);
    }
    for (index, (uid, name)) in programs(&device.features).into_iter().enumerate() {
        let iid = PROGRAM + u64::try_from(index).unwrap_or(0) * 10;
        let label = label_of(&name);
        switches.push(
            switch(iid, aid, &label, suppress, tx, move |on| {
                if on {
                    Command::Program(uid)
                } else {
                    Command::Power(false)
                }
            })
            .await?,
        );
        linked.push(iid);
    }
    let salt = if device
        .features
        .values()
        .any(|feature| salt_name(&feature.name))
    {
        linked.push(SALT);
        Some(filter(SALT, aid, "Salt").await?)
    } else {
        None
    };
    let rinse = if device
        .features
        .values()
        .any(|feature| feature.name.contains("RinseAid"))
    {
        linked.push(RINSE);
        Some(filter(RINSE, aid, "Rinse aid").await?)
    } else {
        None
    };

    let mut power = SwitchService::new(POWER, aid);
    power.set_primary(true);
    if !linked.is_empty() {
        power.set_linked_services(linked);
    }
    set_name(power.name.as_mut(), &device.name).await?;
    if let Some(configured) = power.configured_name.as_mut() {
        device_name(configured, device.id(), &device.name).await?;
    }
    hook_bool(&mut power.power_state, suppress, tx, Command::Power);

    Ok(DishwasherAccessory {
        id: aid,
        info: information(device, &device.name)
            .to_service(1, aid)
            .map_err(|error| anyhow!("{error}"))?,
        power,
        switches,
        salt,
        rinse,
    })
}

pub(super) async fn build_door(device: &BoschDevice) -> Result<ContactSensorAccessory> {
    let label = format!("{} door", device.name);
    let mut door = ContactSensorAccessory::new(device.door_aid, sensor_information(device, &label))
        .map_err(|error| anyhow!("{error}"))?;
    set_name(door.contact_sensor.name.as_mut(), &label).await?;
    door.contact_sensor.status_fault = None;
    door.contact_sensor.status_low_battery = None;
    door.contact_sensor.status_tampered = None;
    Ok(door)
}

pub(super) async fn build_leak(device: &BoschDevice) -> Result<LeakSensorAccessory> {
    let label = format!("{} AquaStop", device.name);
    let mut leak = LeakSensorAccessory::new(device.leak_aid, sensor_information(device, &label))
        .map_err(|error| anyhow!("{error}"))?;
    set_name(leak.leak_sensor.name.as_mut(), &label).await?;
    leak.leak_sensor.status_fault = None;
    leak.leak_sensor.status_low_battery = None;
    leak.leak_sensor.status_tampered = None;
    Ok(leak)
}

pub(super) async fn build_cycle(device: &BoschDevice) -> Result<MotionSensorAccessory> {
    let label = format!("{} cycle", device.name);
    let mut cycle =
        MotionSensorAccessory::new(device.cycle_aid, sensor_information(device, &label))
            .map_err(|error| anyhow!("{error}"))?;
    set_name(cycle.motion_sensor.name.as_mut(), &label).await?;
    cycle.motion_sensor.status_fault = None;
    cycle.motion_sensor.status_low_battery = None;
    cycle.motion_sensor.status_tampered = None;
    Ok(cycle)
}

fn information(device: &BoschDevice, name: &str) -> AccessoryInformation {
    let mut info = sensor_information(device, name);
    // The generated sensor accessories place the next service by counting
    // characteristics, which collides with these sparse ids. The main tile
    // starts its switches at iid 20, so the extras are safe there.
    info.firmware_revision = Some(env!("CARGO_PKG_VERSION").into());
    info.configured_name = Some(name.into());
    info
}

/// Name, maker and model only. Home still shows the name; a rename of the
/// machine itself is stored on the power switch.
fn sensor_information(device: &BoschDevice, name: &str) -> AccessoryInformation {
    let brand = if device.brand.is_empty() {
        "Bosch"
    } else {
        &device.brand
    };
    let model = if device.model.is_empty() {
        "Dishwasher"
    } else {
        &device.model
    };
    AccessoryInformation {
        name: name.into(),
        manufacturer: brand.into(),
        model: model.into(),
        serial_number: if device.ha_id.chars().count() > 1 {
            device.ha_id.clone()
        } else {
            "dishwasher".into()
        },
        ..Default::default()
    }
}

pub(super) async fn apply_main(
    accessory: &mut dyn HapAccessory,
    device: &BoschDevice,
    reading: &Reading,
) -> Result<()> {
    let machine = &reading.machine;
    put_at(
        accessory,
        POWER,
        HapType::PowerState,
        serde_json::json!(machine.power),
    )
    .await?;
    for (key, on) in &machine.options {
        if let Some((_, iid, _)) = option_switches(&device.features)
            .into_iter()
            .find(|(name, _, _)| name == key)
        {
            put_at(accessory, iid, HapType::PowerState, serde_json::json!(*on)).await?;
        }
    }
    for (index, (_, name)) in programs(&device.features).into_iter().enumerate() {
        let iid = PROGRAM + u64::try_from(index).unwrap_or(0) * 10;
        put_at(
            accessory,
            iid,
            HapType::PowerState,
            serde_json::json!(program_on(&machine.program, &name)),
        )
        .await?;
    }
    if let Some(low) = machine.salt_low {
        put_at(
            accessory,
            SALT,
            HapType::FilterChangeIndication,
            serde_json::json!(u8::from(low)),
        )
        .await?;
    }
    if let Some(low) = machine.rinse_low {
        put_at(
            accessory,
            RINSE,
            HapType::FilterChangeIndication,
            serde_json::json!(u8::from(low)),
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn apply_door(accessory: &mut dyn HapAccessory, reading: &Reading) -> Result<()> {
    put(
        accessory,
        HapType::ContactSensor,
        HapType::StatusActive,
        serde_json::json!(reading.online),
    )
    .await?;
    if reading.online {
        let open = u8::from(reading.machine.door_open);
        put(
            accessory,
            HapType::ContactSensor,
            HapType::ContactSensorState,
            serde_json::json!(open),
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn apply_leak(accessory: &mut dyn HapAccessory, reading: &Reading) -> Result<()> {
    put(
        accessory,
        HapType::LeakSensor,
        HapType::StatusActive,
        serde_json::json!(reading.online),
    )
    .await?;
    if reading.online {
        put(
            accessory,
            HapType::LeakSensor,
            HapType::LeakDetected,
            serde_json::json!(u8::from(reading.machine.leak)),
        )
        .await?;
    }
    Ok(())
}

/// `motion` is true only after the cycle has been seen unfinished, then finished.
/// The first reading must not notify, or starting the bridge would.
pub(super) async fn apply_cycle(
    accessory: &mut dyn HapAccessory,
    reading: &Reading,
    motion: bool,
) -> Result<()> {
    put(
        accessory,
        HapType::MotionSensor,
        HapType::StatusActive,
        serde_json::json!(reading.online),
    )
    .await?;
    if reading.online {
        put(
            accessory,
            HapType::MotionSensor,
            HapType::MotionDetected,
            serde_json::json!(motion),
        )
        .await?;
    }
    Ok(())
}

fn program_on(current: &str, feature_name: &str) -> bool {
    if current.is_empty() {
        return false;
    }
    let short = feature_name.rsplit('.').next().unwrap_or(feature_name);
    current == feature_name
        || current == label_of(feature_name)
        || feature_name.ends_with(current)
        || current.ends_with(short)
}

async fn switch(
    iid: u64,
    aid: u64,
    label: &str,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Command>,
    make: impl Fn(bool) -> Command + Send + Sync + 'static,
) -> Result<SwitchService> {
    let mut switch = SwitchService::new(iid, aid);
    set_name(switch.name.as_mut(), label).await?;
    if let Some(configured) = switch.configured_name.as_mut() {
        configured_name(configured, aid, label).await?;
    }
    hook_bool(&mut switch.power_state, suppress, tx, make);
    Ok(switch)
}

async fn filter(iid: u64, aid: u64, label: &str) -> Result<FilterMaintenanceService> {
    let mut filter = FilterMaintenanceService::new(iid, aid);
    filter.filter_life_level = None;
    filter.filter_reset_change_indication = None;
    set_name(filter.name.as_mut(), label).await?;
    Ok(filter)
}

async fn set_name(characteristic: Option<&mut impl HapCharacteristic>, text: &str) -> Result<()> {
    if let Some(characteristic) = characteristic {
        characteristic
            .set_value(serde_json::json!(text))
            .await
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}

async fn put_at(
    accessory: &mut dyn HapAccessory,
    iid: u64,
    kind: HapType,
    value: serde_json::Value,
) -> Result<()> {
    let service = accessory
        .get_mut_services()
        .into_iter()
        .find(|service| service.get_id() == iid);
    if let Some(characteristic) = service.and_then(|service| service.get_mut_characteristic(kind)) {
        characteristic
            .set_value(value)
            .await
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BoschFeature;
    use crate::surfaces::homekit::characteristics::assert_unique_iids;
    use std::collections::BTreeMap;

    fn device() -> BoschDevice {
        let mut features = BTreeMap::new();
        features.insert(
            "10".into(),
            BoschFeature {
                name: "Dishcare.Dishwasher.Program.Intensiv70".into(),
                values: BTreeMap::new(),
            },
        );
        features.insert(
            "30".into(),
            BoschFeature {
                name: "Dishcare.Dishwasher.Program.Eco50".into(),
                values: BTreeMap::new(),
            },
        );
        features.insert(
            "40".into(),
            BoschFeature {
                name: "Dishcare.Dishwasher.Option.ExtraDry".into(),
                values: BTreeMap::new(),
            },
        );
        features.insert(
            "50".into(),
            BoschFeature {
                name: "Dishcare.Dishwasher.Event.SaltNearlyEmpty".into(),
                values: BTreeMap::new(),
            },
        );
        BoschDevice {
            aid: 5,
            door_aid: 6,
            leak_aid: 7,
            cycle_aid: 8,
            name: "Bosch Dishwasher".into(),
            brand: "Bosch".into(),
            model: "SPV2HKX42G".into(),
            ha_id: "BOSCH-HA".into(),
            host: "BOSCH-Dishwasher-BOSCH-HA".into(),
            key: "secret".into(),
            iv: String::new(),
            features,
        }
    }

    #[tokio::test]
    async fn programmes_keep_their_ids_and_nothing_is_reused() {
        let (tx, _rx) = mpsc::channel(1);
        let device = device();
        let main = build_main(&device, &Arc::new(AtomicBool::new(false)), &tx)
            .await
            .unwrap();
        assert_unique_iids(&main);
        let value = serde_json::to_value(&main).unwrap();
        let ids: Vec<u64> = value["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|service| service["iid"].as_u64().unwrap())
            .collect();
        assert!(ids.contains(&POWER));
        assert!(ids.contains(&300));
        assert!(ids.contains(&PROGRAM));
        assert!(ids.contains(&(PROGRAM + 10)));
        assert!(ids.contains(&SALT));
        assert!(!value.to_string().contains("secret"));
        let door = build_door(&device).await.unwrap();
        assert_unique_iids(&door);
        assert_eq!(door.get_id(), 6);
    }
}
