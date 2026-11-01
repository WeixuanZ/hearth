//! The purifier, as one HomeKit accessory on the shared bridge.
//!
//! It carries the fan, air quality, humidity and heat, plus switches Home has
//! no other place for, named as in the Dyson app: Night mode, Jet focus (one
//! concentrated stream rather than a wide one), Continuous monitoring (sensing
//! while the fan is off), and one per oscillation sweep. The switches are
//! linked to the fan, so Home shows them inside it rather than as tiles.

use crate::config::Settings as _;
use crate::connectors::dyson::protocol::{
    air_quality, percent_to_speed, speed_to_percent, sweep, Intent, MachineState, SWEEPS,
};
use crate::{
    devices::purifier::dyson::DysonDevice,
    surfaces::homekit::characteristics::{
        configured_name, device_name, hook_bool, hook_f32, hook_u8, put,
    },
};
use anyhow::{anyhow, Result};
use hap::{
    accessory::{AccessoryInformation, HapAccessory},
    characteristic::HapCharacteristic,
    service::{
        accessory_information::AccessoryInformationService, air_purifier::AirPurifierService,
        air_quality_sensor::AirQualitySensorService, filter_maintenance::FilterMaintenanceService,
        heater_cooler::HeaterCoolerService, humidity_sensor::HumiditySensorService,
        switch::SwitchService, temperature_sensor::TemperatureSensorService, HapService,
    },
    HapType,
};
use serde::ser::{Serialize, SerializeStruct, Serializer};
use std::sync::{atomic::AtomicBool, Arc};
use tokio::sync::mpsc;

pub(super) struct PurifierAccessory {
    id: u64,
    info: AccessoryInformationService,
    purifier: AirPurifierService,
    air: AirQualitySensorService,
    humidity: HumiditySensorService,
    heater: Option<HeaterCoolerService>,
    temperature: Option<TemperatureSensorService>,
    filter: FilterMaintenanceService,
    switches: Vec<SwitchService>,
}

/// Service iids of the switches. Each switch spans three iids.
const NIGHT: u64 = 170;
const FOCUS: u64 = 180;
const SENSE: u64 = 190;
/// The sweep switches follow, one per entry in `SWEEPS`, 10 apart.
const SWEEP: u64 = 200;

fn sweep_iids() -> impl Iterator<Item = (u64, u16)> {
    (SWEEP..).step_by(10).zip(SWEEPS)
}

impl HapAccessory for PurifierAccessory {
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
        let mut services: Vec<&dyn HapService> = vec![
            &self.info,
            &self.purifier,
            &self.air,
            &self.humidity,
            &self.filter,
        ];
        if let Some(heater) = &self.heater {
            services.push(heater);
        }
        if let Some(temperature) = &self.temperature {
            services.push(temperature);
        }
        services.extend(self.switches.iter().map(|switch| switch as &dyn HapService));
        services
    }
    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        let mut services: Vec<&mut dyn HapService> = vec![
            &mut self.info,
            &mut self.purifier,
            &mut self.air,
            &mut self.humidity,
            &mut self.filter,
        ];
        if let Some(heater) = &mut self.heater {
            services.push(heater);
        }
        if let Some(temperature) = &mut self.temperature {
            services.push(temperature);
        }
        services.extend(
            self.switches
                .iter_mut()
                .map(|switch| switch as &mut dyn HapService),
        );
        services
    }
}

impl Serialize for PurifierAccessory {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("HapAccessory", 2)?;
        state.serialize_field("aid", &self.get_id())?;
        state.serialize_field("services", &self.get_services())?;
        state.end()
    }
}

pub(super) async fn build_purifier(
    device: &DysonDevice,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Intent>,
) -> Result<PurifierAccessory> {
    let aid = device.aid;
    let mut purifier = AirPurifierService::new(20, aid);
    purifier.set_primary(true);
    purifier.set_linked_services(
        [NIGHT, FOCUS, SENSE]
            .into_iter()
            .chain(sweep_iids().map(|(iid, _)| iid))
            .collect(),
    );
    purifier.lock_physical_controls = None;
    hook_u8(&mut purifier.active, suppress, tx, |value| {
        Intent::Power(value == 1)
    });
    hook_u8(
        &mut purifier.target_air_purifier_state,
        suppress,
        tx,
        |value| Intent::Auto(value == 1),
    );
    if let Some(speed) = purifier.rotation_speed.as_mut() {
        // Ten speeds, so the slider snaps to 10 % steps.
        limit(speed, None, None, Some(10.0))?;
        hook_f32(speed, suppress, tx, |percent| {
            Intent::Speed(percent_to_speed(percent))
        });
    }
    if let Some(swing) = purifier.swing_mode.as_mut() {
        hook_u8(swing, suppress, tx, |value| Intent::Swing(value == 1));
    }

    let mut air = AirQualitySensorService::new(40, aid);
    air.nitrogen_dioxide_density = None;
    air.ozone_density = None;
    air.sulphur_dioxide_density = None;
    air.volatile_organic_compound_density = None;
    air.status_fault = None;
    air.status_low_battery = None;
    air.status_tampered = None;

    let mut humidity = HumiditySensorService::new(70, aid);
    humidity.status_fault = None;
    humidity.status_low_battery = None;
    humidity.status_tampered = None;

    let (mut heater, mut temperature) = if device.heating {
        let mut heater = HeaterCoolerService::new(100, aid);
        heater.lock_physical_controls = None;
        heater.rotation_speed = None;
        heater.swing_mode = None;
        heater.cooling_threshold_temperature = None;
        hook_u8(&mut heater.active, suppress, tx, |value| {
            Intent::Heat(value == 1)
        });
        // The fan only heats. Offering Auto or Cool would read as heat off.
        HapCharacteristic::set_valid_values(
            &mut heater.target_heater_cooler_state,
            Some(vec![serde_json::json!(1)]),
        )
        .map_err(|error| anyhow!("{error}"))?;
        heater
            .target_heater_cooler_state
            .set_value(serde_json::json!(1))
            .await
            .map_err(|error| anyhow!("{error}"))?;
        hook_u8(
            &mut heater.target_heater_cooler_state,
            suppress,
            tx,
            |value| Intent::Heat(value == 1),
        );
        if let Some(target) = heater.heating_threshold_temperature.as_mut() {
            // The fan heats to 1–37 °C. Home's default stops at 25.
            limit(target, Some(1.0), Some(37.0), Some(1.0))?;
            hook_f32(target, suppress, tx, Intent::HeatTargetC);
        }
        (Some(heater), None)
    } else {
        let mut temperature = TemperatureSensorService::new(100, aid);
        temperature.status_fault = None;
        temperature.status_low_battery = None;
        temperature.status_tampered = None;
        (None, Some(temperature))
    };

    let mut filter = FilterMaintenanceService::new(140, aid);
    filter.filter_reset_change_indication = None;

    // hap sends an empty Name otherwise, which Home refuses.
    name(purifier.name.as_mut(), &device.name).await?;
    if let Some(configured) = purifier.configured_name.as_mut() {
        device_name(configured, device.id(), &device.name).await?;
    }
    name(air.name.as_mut(), "Air quality").await?;
    name(humidity.name.as_mut(), "Humidity").await?;
    name(filter.name.as_mut(), "Filter").await?;
    if let Some(heater) = heater.as_mut() {
        name(heater.name.as_mut(), "Heater").await?;
    }
    if let Some(temperature) = temperature.as_mut() {
        name(temperature.name.as_mut(), "Temperature").await?;
    }

    let mut switches = vec![
        switch(NIGHT, aid, "Night mode", suppress, tx, Intent::Night).await?,
        switch(FOCUS, aid, "Jet focus", suppress, tx, Intent::Focus).await?,
        switch(
            SENSE,
            aid,
            "Continuous monitoring",
            suppress,
            tx,
            Intent::Monitor,
        )
        .await?,
    ];
    // Home has no oscillation angle, so each of the app's sweeps is a switch.
    // Turning one off stops oscillation.
    for (iid, width) in sweep_iids() {
        let label = format!("Sweep {width}");
        let make = move |on| {
            if on {
                Intent::Sweep(width)
            } else {
                Intent::Swing(false)
            }
        };
        switches.push(switch(iid, aid, &label, suppress, tx, make).await?);
    }

    Ok(PurifierAccessory {
        id: aid,
        info: AccessoryInformation {
            name: device.name.clone(),
            manufacturer: "Dyson".into(),
            model: device.model.clone(),
            serial_number: device.serial.clone(),
            firmware_revision: Some("1.0".into()),
            ..Default::default()
        }
        .to_service(1, aid)
        .map_err(|error| anyhow!("{error}"))?,
        purifier,
        air,
        humidity,
        heater,
        temperature,
        filter,
        switches,
    })
}

async fn switch(
    iid: u64,
    aid: u64,
    label: &str,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Intent>,
    make: impl Fn(bool) -> Intent + Send + Sync + 'static,
) -> Result<SwitchService> {
    let mut switch = SwitchService::new(iid, aid);
    name(switch.name.as_mut(), label).await?;
    if let Some(name) = switch.configured_name.as_mut() {
        configured_name(name, aid, label).await?;
    }
    hook_bool(&mut switch.power_state, suppress, tx, make);
    Ok(switch)
}

async fn name(characteristic: Option<&mut impl HapCharacteristic>, text: &str) -> Result<()> {
    if let Some(characteristic) = characteristic {
        characteristic
            .set_value(serde_json::json!(text))
            .await
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}

/// Narrows a number characteristic to what the fan accepts.
fn limit(
    characteristic: &mut impl HapCharacteristic,
    min: Option<f32>,
    max: Option<f32>,
    step: Option<f32>,
) -> Result<()> {
    let to_json = |value: Option<f32>| value.map(|value| serde_json::json!(value));
    if min.is_some() {
        HapCharacteristic::set_min_value(characteristic, to_json(min))
            .map_err(|error| anyhow!("{error}"))?;
    }
    if max.is_some() {
        HapCharacteristic::set_max_value(characteristic, to_json(max))
            .map_err(|error| anyhow!("{error}"))?;
    }
    if step.is_some() {
        HapCharacteristic::set_step_value(characteristic, to_json(step))
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}

pub(super) async fn apply_purifier(
    accessory: &mut dyn HapAccessory,
    state: &MachineState,
) -> Result<()> {
    let on = if state.power_on { 1 } else { 0 };
    put(
        accessory,
        HapType::AirPurifier,
        HapType::Active,
        serde_json::json!(on),
    )
    .await?;
    put(
        accessory,
        HapType::AirPurifier,
        HapType::CurrentAirPurifierState,
        serde_json::json!(if state.power_on { 2 } else { 0 }),
    )
    .await?;
    put(
        accessory,
        HapType::AirPurifier,
        HapType::TargetAirPurifierState,
        serde_json::json!(if state.auto { 1 } else { 0 }),
    )
    .await?;
    if let Some(speed) = state.speed {
        put(
            accessory,
            HapType::AirPurifier,
            HapType::RotationSpeed,
            serde_json::json!(speed_to_percent(speed)),
        )
        .await?;
    }
    put(
        accessory,
        HapType::AirPurifier,
        HapType::SwingMode,
        serde_json::json!(if state.oscillating { 1 } else { 0 }),
    )
    .await?;

    put(
        accessory,
        HapType::AirQualitySensor,
        HapType::CurrentAirQuality,
        serde_json::json!(air_quality(state)),
    )
    .await?;
    if let Some(pm) = state.pm25 {
        put(
            accessory,
            HapType::AirQualitySensor,
            HapType::Pm2_5Density,
            serde_json::json!(pm),
        )
        .await?;
    }
    if let Some(pm) = state.pm10 {
        put(
            accessory,
            HapType::AirQualitySensor,
            HapType::Pm10Density,
            serde_json::json!(pm),
        )
        .await?;
    }
    // Home greys out a sensor that is not active, rather than show a reading
    // the fan stopped taking.
    let sensing = state.sensing();
    put(
        accessory,
        HapType::AirQualitySensor,
        HapType::StatusActive,
        serde_json::json!(sensing && (state.pm25.is_some() || state.pm10.is_some())),
    )
    .await?;

    if let Some(humidity) = state.humidity {
        put(
            accessory,
            HapType::HumiditySensor,
            HapType::CurrentRelativeHumidity,
            serde_json::json!(humidity),
        )
        .await?;
    }
    put(
        accessory,
        HapType::HumiditySensor,
        HapType::StatusActive,
        serde_json::json!(sensing && state.humidity.is_some()),
    )
    .await?;

    if let Some(celsius) = state.temperature_c {
        put(
            accessory,
            HapType::HeaterCooler,
            HapType::CurrentTemperature,
            serde_json::json!(celsius),
        )
        .await?;
        put(
            accessory,
            HapType::TemperatureSensor,
            HapType::CurrentTemperature,
            serde_json::json!(celsius),
        )
        .await?;
    }
    put(
        accessory,
        HapType::TemperatureSensor,
        HapType::StatusActive,
        serde_json::json!(sensing && state.temperature_c.is_some()),
    )
    .await?;

    let current = if !state.heat_on {
        0
    } else if state.heating_now {
        2
    } else {
        1
    };
    put(
        accessory,
        HapType::HeaterCooler,
        HapType::Active,
        serde_json::json!(if state.heat_on { 1 } else { 0 }),
    )
    .await?;
    put(
        accessory,
        HapType::HeaterCooler,
        HapType::CurrentHeaterCoolerState,
        serde_json::json!(current),
    )
    .await?;
    put(
        accessory,
        HapType::HeaterCooler,
        HapType::TargetHeaterCoolerState,
        // Always heat. Whether it is heating is Active, above.
        serde_json::json!(1),
    )
    .await?;
    if let Some(target) = state.heat_target_c {
        put(
            accessory,
            HapType::HeaterCooler,
            HapType::HeatingThresholdTemperature,
            serde_json::json!(target.clamp(1.0, 37.0)),
        )
        .await?;
    }

    if let Some(life) = state.hepa_life {
        put(
            accessory,
            HapType::FilterMaintenance,
            HapType::FilterLifeLevel,
            serde_json::json!(life),
        )
        .await?;
        put(
            accessory,
            HapType::FilterMaintenance,
            HapType::FilterChangeIndication,
            serde_json::json!(if life < 15 { 1 } else { 0 }),
        )
        .await?;
    }

    for (iid, on) in [
        (NIGHT, state.night),
        (FOCUS, state.focus),
        (SENSE, state.monitoring),
    ] {
        put_switch(accessory, iid, on).await?;
    }
    // The preset nearest the fan's sweep is on while it oscillates.
    let nearest = sweep(state).and_then(|current| {
        SWEEPS
            .into_iter()
            .min_by_key(|width| width.abs_diff(current))
    });
    for (iid, width) in sweep_iids() {
        put_switch(accessory, iid, state.oscillating && nearest == Some(width)).await?;
    }
    Ok(())
}

/// The switches share a service type, so they are found by iid.
async fn put_switch(accessory: &mut dyn HapAccessory, iid: u64, on: bool) -> Result<()> {
    let service = accessory
        .get_mut_services()
        .into_iter()
        .find(|service| service.get_id() == iid);
    if let Some(power) =
        service.and_then(|service| service.get_mut_characteristic(HapType::PowerState))
    {
        power
            .set_value(serde_json::json!(on))
            .await
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surfaces::homekit::characteristics::assert_unique_iids;

    #[tokio::test]
    async fn sensors_are_inactive_unless_the_fan_is_sensing() {
        let device = DysonDevice {
            aid: 3,
            name: "Purifier".into(),
            serial: "ABC-XY-12345678".into(),
            model: "XY01".into(),
            product_type: "000X".into(),
            mqtt_password: "x".into(),
            host: String::new(),
            port: 1883,
            heating: false,
        };
        let suppress = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel(4);
        let mut purifier = build_purifier(&device, &suppress, &tx).await.unwrap();
        // Status Active (75) on air quality (8D), humidity (82), temperature (8A).
        let active = |purifier: &PurifierAccessory| -> Vec<bool> {
            let value = serde_json::to_value(purifier).unwrap();
            ["8D", "82", "8A"]
                .map(|kind| {
                    let service = value["services"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|service| service["type"] == kind)
                        .unwrap();
                    service["characteristics"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|characteristic| characteristic["type"] == "75")
                        .unwrap()["value"]
                        == true
                })
                .to_vec()
        };
        let mut state = MachineState {
            pm25: Some(4.0),
            humidity: Some(40.0),
            temperature_c: Some(21.0),
            ..Default::default()
        };
        // Off, and not monitoring: the readings are old.
        apply_purifier(&mut purifier, &state).await.unwrap();
        assert_eq!(active(&purifier), [false, false, false]);
        // Off, but monitoring: the fan still measures.
        state.monitoring = true;
        apply_purifier(&mut purifier, &state).await.unwrap();
        assert_eq!(active(&purifier), [true, true, true]);
    }

    #[tokio::test]
    async fn iids_are_unique() {
        let mut device = DysonDevice {
            aid: 3,
            name: "Purifier".into(),
            serial: "ABC-XY-12345678".into(),
            model: "XY01".into(),
            product_type: "000X".into(),
            mqtt_password: "x".into(),
            host: String::new(),
            port: 1883,
            heating: true,
        };
        let suppress = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel(4);
        assert_unique_iids(&build_purifier(&device, &suppress, &tx).await.unwrap());
        device.heating = false;
        assert_unique_iids(&build_purifier(&device, &suppress, &tx).await.unwrap());
    }

    #[tokio::test]
    async fn switches_live_in_the_purifier() {
        let device = DysonDevice {
            aid: 3,
            name: "Purifier".into(),
            serial: "ABC-XY-12345678".into(),
            model: "XY01".into(),
            product_type: "000X".into(),
            mqtt_password: "x".into(),
            host: String::new(),
            port: 1883,
            heating: true,
        };
        let suppress = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel(4);
        let mut purifier = build_purifier(&device, &suppress, &tx).await.unwrap();
        let state = MachineState {
            focus: true,
            oscillating: true,
            oscillation_low: Some(56),
            oscillation_high: Some(144),
            ..Default::default()
        };
        apply_purifier(&mut purifier, &state).await.unwrap();

        let value = serde_json::to_value(&purifier).unwrap();
        let services = value["services"].as_array().unwrap();
        let fan = services
            .iter()
            .find(|service| service["iid"] == 20)
            .unwrap();
        assert_eq!(
            fan["linked"],
            serde_json::json!([NIGHT, FOCUS, SENSE, 200, 210, 220, 230])
        );
        let power = |iid: u64| {
            let service = services
                .iter()
                .find(|service| service["iid"] == iid)
                .unwrap();
            service["characteristics"][0]["value"].clone()
        };
        assert_eq!(power(NIGHT), serde_json::json!(false));
        assert_eq!(power(FOCUS), serde_json::json!(true));
        assert_eq!(power(SENSE), serde_json::json!(false));
        // An 88 degree sweep lights the 90 preset only.
        let sweeps: Vec<_> = (200..=230).step_by(10).map(power).collect();
        assert_eq!(
            sweeps,
            [false, true, false, false].map(|on| serde_json::json!(on))
        );

        let characteristic = |kind: &str| {
            services
                .iter()
                .flat_map(|service| service["characteristics"].as_array().unwrap())
                .find(|characteristic| characteristic["type"] == kind)
                .unwrap()
                .clone()
        };
        let threshold = characteristic("12");
        assert_eq!(
            (
                &threshold["minValue"],
                &threshold["maxValue"],
                &threshold["minStep"]
            ),
            (
                &serde_json::json!(1.0),
                &serde_json::json!(37.0),
                &serde_json::json!(1.0)
            )
        );
        let target = characteristic("B2");
        assert_eq!(target["valid-values"], serde_json::json!([1]));
        assert_eq!(target["value"], serde_json::json!(1));
    }
}
