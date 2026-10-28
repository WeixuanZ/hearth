//! The purifier, as HomeKit accessories on the shared bridge.
//!
//! The bridge carries the purifier (fan, air quality, humidity, heat) plus three
//! switches Home has no other place for: night mode, the focused jet, and
//! sensing while the fan is off.

use super::protocol::{air_quality, percent_to_speed, speed_to_percent, Intent, MachineState};
use crate::{
    config::DysonDevice,
    hapset::{hook_bool, hook_f32, hook_u8, put},
};
use anyhow::{anyhow, Result};
use hap::{
    accessory::{switch::SwitchAccessory, AccessoryInformation, HapAccessory},
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

fn info(device: &DysonDevice, name: &str, suffix: &str) -> AccessoryInformation {
    AccessoryInformation {
        name: name.into(),
        manufacturer: "Dyson".into(),
        model: device.model.clone(),
        serial_number: format!("{}{suffix}", device.serial),
        firmware_revision: Some("1.0".into()),
        ..Default::default()
    }
}

pub(super) fn build_purifier(
    device: &DysonDevice,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Intent>,
) -> Result<PurifierAccessory> {
    let mut purifier = AirPurifierService::new(20, 2);
    purifier.set_primary(true);
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
        hook_f32(speed, suppress, tx, |percent| {
            Intent::Speed(percent_to_speed(percent))
        });
    }
    if let Some(swing) = purifier.swing_mode.as_mut() {
        hook_u8(swing, suppress, tx, |value| Intent::Swing(value == 1));
    }

    let mut air = AirQualitySensorService::new(40, 2);
    air.nitrogen_dioxide_density = None;
    air.ozone_density = None;
    air.sulphur_dioxide_density = None;
    air.volatile_organic_compound_density = None;
    air.status_fault = None;
    air.status_low_battery = None;
    air.status_tampered = None;

    let mut humidity = HumiditySensorService::new(70, 2);
    humidity.status_fault = None;
    humidity.status_low_battery = None;
    humidity.status_tampered = None;

    let (heater, temperature) = if device.heating {
        let mut heater = HeaterCoolerService::new(100, 2);
        heater.lock_physical_controls = None;
        heater.rotation_speed = None;
        heater.swing_mode = None;
        heater.cooling_threshold_temperature = None;
        hook_u8(&mut heater.active, suppress, tx, |value| {
            Intent::Heat(value == 1)
        });
        hook_u8(
            &mut heater.target_heater_cooler_state,
            suppress,
            tx,
            |value| Intent::Heat(value == 1),
        );
        if let Some(target) = heater.heating_threshold_temperature.as_mut() {
            hook_f32(target, suppress, tx, Intent::HeatTargetC);
        }
        (Some(heater), None)
    } else {
        let mut temperature = TemperatureSensorService::new(100, 2);
        temperature.status_fault = None;
        temperature.status_low_battery = None;
        temperature.status_tampered = None;
        (None, Some(temperature))
    };

    let mut filter = FilterMaintenanceService::new(140, 2);
    filter.filter_reset_change_indication = None;

    Ok(PurifierAccessory {
        id: device.aid,
        info: info(device, &device.name, "")
            .to_service(1, device.aid)
            .map_err(|error| anyhow!("{error}"))?,
        purifier,
        air,
        humidity,
        heater,
        temperature,
        filter,
    })
}

pub(super) fn named_switch(
    id: u64,
    device: &DysonDevice,
    name: &str,
    suffix: &str,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Intent>,
    make: impl Fn(bool) -> Intent + Send + Sync + 'static,
) -> Result<SwitchAccessory> {
    let mut switch =
        SwitchAccessory::new(id, info(device, name, suffix)).map_err(|error| anyhow!("{error}"))?;
    // hap puts the switch right after the information service, at iid 8, but
    // the firmware revision sits at a fixed iid 10. Home rejects the clash.
    switch.switch = SwitchService::new(20, id);
    hook_bool(&mut switch.switch.power_state, suppress, tx, make);
    Ok(switch)
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
    put(
        accessory,
        HapType::AirQualitySensor,
        HapType::StatusActive,
        serde_json::json!(state.monitoring || state.power_on || state.pm25.is_some()),
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
        serde_json::json!(state.humidity.is_some()),
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
        put(
            accessory,
            HapType::TemperatureSensor,
            HapType::StatusActive,
            serde_json::json!(true),
        )
        .await?;
    }

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
        serde_json::json!(if state.heat_on { 1 } else { 0 }),
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
    Ok(())
}

pub(super) async fn apply_switch(accessory: &mut dyn HapAccessory, on: bool) -> Result<()> {
    put(
        accessory,
        HapType::Switch,
        HapType::PowerState,
        serde_json::json!(on),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hapset::assert_unique_iids;

    #[test]
    fn iids_are_unique() {
        let mut device = DysonDevice {
            aid: 3,
            name: "Purifier".into(),
            serial: "ABC-DE-FGH1234A".into(),
            model: "XY01".into(),
            product_type: "000X".into(),
            mqtt_password: "x".into(),
            host: String::new(),
            port: 1883,
            heating: true,
        };
        let suppress = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel(4);
        assert_unique_iids(&build_purifier(&device, &suppress, &tx).unwrap());
        assert_unique_iids(
            &named_switch(4, &device, "Night", "-night", &suppress, &tx, Intent::Night).unwrap(),
        );
        device.heating = false;
        assert_unique_iids(&build_purifier(&device, &suppress, &tx).unwrap());
    }
}
