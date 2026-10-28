//! The purifier, as one HomeKit bridge.
//!
//! Apple Home will only show a device that stays on the LAN and speaks HomeKit.
//! This process is that device. The bridge carries the purifier (fan, air
//! quality, humidity, heat) plus three switches Home has no other place for:
//! night mode, the focused jet, and sensing while the fan is off.

use crate::{
    config::{self, Config},
    protocol::{air_quality, percent_to_speed, speed_to_percent, Intent, MachineState},
};
use anyhow::{anyhow, Context, Result};
use hap::{
    accessory::{
        bridge::BridgeAccessory, switch::SwitchAccessory, AccessoryCategory, AccessoryInformation,
        HapAccessory,
    },
    characteristic::CharacteristicCallbacks,
    server::{IpServer, Server},
    service::{
        accessory_information::AccessoryInformationService, air_purifier::AirPurifierService,
        air_quality_sensor::AirQualitySensorService, filter_maintenance::FilterMaintenanceService,
        heater_cooler::HeaterCoolerService, humidity_sensor::HumiditySensorService,
        temperature_sensor::TemperatureSensorService, HapService,
    },
    storage::{FileStorage, Storage},
    Config as HapConfig, HapType, MacAddress, Pin,
};
use serde::ser::{Serialize, SerializeStruct, Serializer};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{mpsc, watch};

struct PurifierAccessory {
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

fn device_id(serial: &str) -> [u8; 6] {
    let digest = Sha256::digest(serial.as_bytes());
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&digest[..6]);
    mac[0] = (mac[0] | 0x02) & 0xFE;
    mac
}

fn hook_u8(
    characteristic: &mut impl CharacteristicCallbacks<u8>,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Intent>,
    make: impl Fn(u8) -> Intent + Send + Sync + 'static,
) {
    let suppress = suppress.clone();
    let tx = tx.clone();
    characteristic.on_update(Some(move |_old: &u8, new: &u8| {
        if !suppress.load(Ordering::SeqCst) {
            let _ = tx.try_send(make(*new));
        }
        Ok(())
    }));
}

fn hook_bool(
    characteristic: &mut impl CharacteristicCallbacks<bool>,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Intent>,
    make: impl Fn(bool) -> Intent + Send + Sync + 'static,
) {
    let suppress = suppress.clone();
    let tx = tx.clone();
    characteristic.on_update(Some(move |_old: &bool, new: &bool| {
        if !suppress.load(Ordering::SeqCst) {
            let _ = tx.try_send(make(*new));
        }
        Ok(())
    }));
}

fn hook_f32(
    characteristic: &mut impl CharacteristicCallbacks<f32>,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Intent>,
    make: impl Fn(f32) -> Intent + Send + Sync + 'static,
) {
    let suppress = suppress.clone();
    let tx = tx.clone();
    characteristic.on_update(Some(move |old: &f32, new: &f32| {
        if !suppress.load(Ordering::SeqCst) && (old - new).abs() >= 0.2 {
            let _ = tx.try_send(make(*new));
        }
        Ok(())
    }));
}

fn info(config: &Config, name: &str, suffix: &str) -> AccessoryInformation {
    AccessoryInformation {
        name: name.into(),
        manufacturer: "Dyson".into(),
        model: config.model.clone(),
        serial_number: format!("{}{suffix}", config.serial),
        firmware_revision: Some("1.0".into()),
        ..Default::default()
    }
}

fn build_purifier(
    config: &Config,
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

    let (heater, temperature) = if config.heating {
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
        id: 2,
        info: info(config, &config.name, "")
            .to_service(1, 2)
            .map_err(|error| anyhow!("{error}"))?,
        purifier,
        air,
        humidity,
        heater,
        temperature,
        filter,
    })
}

fn named_switch(
    id: u64,
    config: &Config,
    name: &str,
    suffix: &str,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Intent>,
    make: impl Fn(bool) -> Intent + Send + Sync + 'static,
) -> Result<SwitchAccessory> {
    let mut switch =
        SwitchAccessory::new(id, info(config, name, suffix)).map_err(|error| anyhow!("{error}"))?;
    hook_bool(&mut switch.switch.power_state, suppress, tx, make);
    Ok(switch)
}

async fn put(
    accessory: &mut dyn HapAccessory,
    service: HapType,
    kind: HapType,
    value: serde_json::Value,
) -> Result<()> {
    let Some(service) = accessory.get_mut_service(service) else {
        return Ok(());
    };
    if let Some(characteristic) = service.get_mut_characteristic(kind) {
        characteristic
            .set_value(value)
            .await
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}

async fn apply_purifier(accessory: &mut dyn HapAccessory, state: &MachineState) -> Result<()> {
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

async fn apply_switch(accessory: &mut dyn HapAccessory, on: bool) -> Result<()> {
    put(
        accessory,
        HapType::Switch,
        HapType::PowerState,
        serde_json::json!(on),
    )
    .await
}

pub async fn serve(
    config: Config,
    mut states: watch::Receiver<MachineState>,
    intents: mpsc::Sender<Intent>,
) -> Result<()> {
    let suppress = Arc::new(AtomicBool::new(false));
    let bridge = BridgeAccessory::new(1, info(&config, &config.name, "-bridge"))
        .map_err(|error| anyhow!("{error}"))?;
    let purifier = build_purifier(&config, &suppress, &intents)?;
    let night = named_switch(
        3,
        &config,
        "Night mode",
        "-night",
        &suppress,
        &intents,
        Intent::Night,
    )?;
    let focus = named_switch(
        4,
        &config,
        "Focused jet",
        "-focus",
        &suppress,
        &intents,
        Intent::Focus,
    )?;
    let monitor = named_switch(
        5,
        &config,
        "Keep sensing",
        "-sense",
        &suppress,
        &intents,
        Intent::Monitor,
    )?;

    let dir = config::homekit_dir()?;
    std::fs::create_dir_all(&dir)?;
    let mut storage = FileStorage::new(dir.to_str().context("homekit path")?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    let pin =
        Pin::new(config::parse_pin(&config.homekit_pin)?).map_err(|error| anyhow!("{error}"))?;
    let hap_config = match storage.load_config().await {
        Ok(mut existing) => {
            existing.redetermine_local_ip();
            existing.name = config.name.clone();
            existing.category = AccessoryCategory::Bridge;
            storage
                .save_config(&existing)
                .await
                .map_err(|error| anyhow!("{error}"))?;
            existing
        }
        Err(_) => {
            let created = HapConfig {
                pin,
                name: config.name.clone(),
                device_id: MacAddress::new(device_id(&config.serial)),
                category: AccessoryCategory::Bridge,
                ..Default::default()
            };
            storage
                .save_config(&created)
                .await
                .map_err(|error| anyhow!("{error}"))?;
            created
        }
    };

    let server = IpServer::new(hap_config, storage)
        .await
        .map_err(|error| anyhow!("HomeKit server failed to start: {error}"))?;
    server
        .add_accessory(bridge)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    let purifier = server
        .add_accessory(purifier)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    let night = server
        .add_accessory(night)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    let focus = server
        .add_accessory(focus)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    let monitor = server
        .add_accessory(monitor)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    let runtime = server.run_handle();

    let updates = async move {
        loop {
            let current = states.borrow().clone();
            suppress.store(true, Ordering::SeqCst);
            let result = async {
                apply_purifier(purifier.lock().await.as_mut(), &current).await?;
                apply_switch(night.lock().await.as_mut(), current.night).await?;
                apply_switch(focus.lock().await.as_mut(), current.focus).await?;
                apply_switch(monitor.lock().await.as_mut(), current.monitoring).await?;
                Ok::<(), anyhow::Error>(())
            }
            .await;
            suppress.store(false, Ordering::SeqCst);
            result?;
            if states.changed().await.is_err() {
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    tokio::pin!(runtime);
    tokio::select! {
        result = &mut runtime => result.map_err(|error| anyhow!("HomeKit stopped: {error}"))?,
        result = updates => result?,
    }
    Ok(())
}
