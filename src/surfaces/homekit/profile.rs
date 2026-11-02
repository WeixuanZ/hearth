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
        leak_sensor::LeakSensorService, motion_sensor::MotionSensorService,
        speaker::SpeakerService, switch::SwitchService, television::TelevisionService,
        temperature_sensor::TemperatureSensorService, HapService,
    },
    HapType,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;

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

    async fn tv(&mut self) -> Result<()> {
        let aid = self.aid("main");
        let mut tv = TelevisionService::new(self.iid("main", "television"), aid);
        tv.set_primary(true);
        tv.display_order = None;
        tv.closed_captions = None;
        tv.current_media_state = None;
        tv.target_media_state = None;
        tv.picture_mode = None;
        tv.power_mode_selection = None;
        self.wiring.read(aid, &tv.active, |snapshot| {
            Some(flag(on(snapshot, "power")))
        });
        self.wiring.write(&mut tv.active, |value: u8| {
            vec![("power", json!(value == 1))]
        });

        let mut services: Vec<Box<dyn HapService>> = Vec::new();
        let mut linked = Vec::new();
        if let Some(input) = self.role(Role::Input) {
            let Kind::Choice { options } = input.kind.clone() else {
                unreachable!("an input is a choice")
            };
            let numbered: Vec<(String, u32)> = options
                .iter()
                .enumerate()
                .map(|(index, option)| {
                    (option.id.clone(), option.number.unwrap_or(index as u32 + 1))
                })
                .collect();
            let shown = numbered.clone();
            self.wiring
                .read(aid, &tv.active_identifier, move |snapshot| {
                    let current = value(snapshot, input.id)?;
                    shown
                        .iter()
                        .find(|(id, _)| Some(id.as_str()) == current.as_str())
                        .map(|(_, number)| json!(number))
                });
            self.wiring
                .write_seeing(&mut tv.active_identifier, move |number: u32, now| {
                    // Home's picker works only while the TV is on. With it off, an
                    // input comes from a scene or an automation, beside turning it
                    // off, and waking the TV for it would undo that.
                    match numbered.iter().find(|(_, n)| *n == number) {
                        Some((option, _)) if on(now, "power") => vec![(input.id, json!(option))],
                        _ => Vec::new(),
                    }
                });
            for (index, option) in options.iter().enumerate() {
                let iid = self.iid("main", &format!("input:{}", option.id));
                let mut source = InputSourceService::new(iid, aid);
                configured_name(&mut source.configured_name, aid, &option.name).await?;
                fixed(Some(&mut source.name), json!(option.name)).await?;
                // A tuner for the TV's own channels; HDMI by its name; else other.
                let kind = if option.id == "tv" {
                    2
                } else if option.id.starts_with("hdmi") {
                    3
                } else {
                    0
                };
                fixed(Some(&mut source.input_source_type), json!(kind)).await?;
                fixed(Some(&mut source.is_configured), json!(1u8)).await?;
                fixed(Some(&mut source.current_visibility_state), json!(0u8)).await?;
                fixed(
                    source.identifier.as_mut(),
                    json!(option.number.unwrap_or(index as u32 + 1)),
                )
                .await?;
                fixed(source.input_device_type.as_mut(), json!(1u8)).await?;
                fixed(source.target_visibility_state.as_mut(), json!(0u8)).await?;
                linked.push(iid);
                services.push(Box::new(source));
            }
        }
        let remote = self.role(Role::Remote);
        let keys: Vec<String> = match remote.as_ref().map(|remote| &remote.kind) {
            Some(Kind::Keys { keys }) => keys.iter().map(|key| key.id.clone()).collect(),
            _ => Vec::new(),
        };
        if let Some(remote) = &remote {
            let (id, offered) = (remote.id, keys.clone());
            self.wiring.write(&mut tv.remote_key, move |code: u8| {
                // Home's remote keys, by HAP's numbers.
                let key = match code {
                    0 => "rewind",
                    1 => "fast_forward",
                    4 => "up",
                    5 => "down",
                    6 => "left",
                    7 => "right",
                    8 => "select",
                    9 | 10 => "back",
                    11 => "play",
                    15 => "info",
                    _ => return Vec::new(),
                };
                if offered.iter().any(|offered| offered == key) {
                    vec![(id, json!(key))]
                } else {
                    Vec::new()
                }
            });
        }
        match self.role(Role::Brightness) {
            Some(level) => {
                if let Some(brightness) = tv.brightness.as_mut() {
                    self.wiring.read(aid, brightness, move |snapshot| {
                        number(snapshot, level.id).map(|value| json!(value.round() as i32))
                    });
                    self.wiring
                        .write(brightness, move |value: i32| vec![(level.id, json!(value))]);
                }
            }
            None => tv.brightness = None,
        }
        self.name_main(tv.name.as_mut(), &mut tv.configured_name)
            .await?;

        // Volume, when the remote has it: Home's buttons press its keys.
        if keys.iter().any(|key| key == "volume_up") {
            let id = remote.as_ref().map_or("remote", |remote| remote.id);
            let mut speaker = SpeakerService::new(self.iid("main", "speaker"), aid);
            speaker.volume = None;
            speaker.active = None;
            self.wiring
                .write(&mut speaker.mute, move |_: bool| vec![(id, json!("mute"))]);
            fixed(speaker.volume_control_type.as_mut(), json!(1u8)).await?;
            if let Some(selector) = speaker.volume_selector.as_mut() {
                // 0 is louder, as the HAP spec has it and Control Center sends.
                self.wiring.write(selector, move |value: u8| {
                    vec![(
                        id,
                        json!(if value == 0 {
                            "volume_up"
                        } else {
                            "volume_down"
                        }),
                    )]
                });
            }
            services.insert(0, Box::new(speaker));
        }
        tv.set_linked_services(linked);
        let info = self.information(aid, &self.device.name.clone(), "", true)?;
        let mut all: Vec<Box<dyn HapService>> = vec![Box::new(info), Box::new(tv)];
        all.extend(services);
        self.accessory(aid, all, HapType::Television, None);

        // Home shows a TV as power and inputs only, so each switch beside it
        // is a tile of its own.
        for capability in self.device.capabilities.clone() {
            if !matches!(capability.kind, Kind::Switch) {
                continue;
            }
            let aid = self.aid(capability.id);
            let switch = self
                .switch(
                    capability.id,
                    aid,
                    "switch",
                    &capability.name,
                    capability.id,
                )
                .await?;
            let info = self.information(aid, &capability.name, capability.id, true)?;
            self.accessories.push(Accessory {
                id: aid,
                services: vec![Box::new(info), Box::new(switch)],
            });
        }
        Ok(())
    }

    async fn purifier(&mut self) -> Result<()> {
        let aid = self.aid("main");
        let mut purifier = AirPurifierService::new(self.iid("main", "purifier"), aid);
        purifier.set_primary(true);
        purifier.lock_physical_controls = None;
        self.wiring.read(aid, &purifier.active, |snapshot| {
            Some(flag(on(snapshot, "power")))
        });
        self.wiring.write(&mut purifier.active, |value: u8| {
            vec![("power", json!(value == 1))]
        });
        self.wiring
            .read(aid, &purifier.current_air_purifier_state, |snapshot| {
                Some(json!(if on(snapshot, "power") { 2 } else { 0 }))
            });
        if let Some(auto) = self.role(Role::Auto) {
            self.wiring
                .read(aid, &purifier.target_air_purifier_state, move |snapshot| {
                    Some(flag(on(snapshot, auto.id)))
                });
            self.wiring
                .write(&mut purifier.target_air_purifier_state, move |value: u8| {
                    vec![(auto.id, json!(value == 1))]
                });
        }
        match (self.role(Role::FanSpeed), purifier.rotation_speed.as_mut()) {
            (Some(speed), Some(rotation)) => {
                let Kind::Level { max, step, .. } = speed.kind else {
                    unreachable!("a speed is a level")
                };
                // As a share of the fastest, in the speed's own steps.
                limit(rotation, None, None, Some(100.0 * step / max))?;
                self.wiring.read(aid, rotation, move |snapshot| {
                    number(snapshot, speed.id).map(|value| json!(value as f32 / max * 100.0))
                });
                self.wiring.write(rotation, move |percent: f32| {
                    vec![(speed.id, json!((percent / 100.0 * max).round().max(step)))]
                });
            }
            _ => purifier.rotation_speed = None,
        }
        match (self.role(Role::Oscillation), purifier.swing_mode.as_mut()) {
            (Some(swing), Some(mode)) => {
                self.wiring.read(aid, mode, move |snapshot| {
                    Some(flag(on(snapshot, swing.id)))
                });
                self.wiring
                    .write(mode, move |value: u8| vec![(swing.id, json!(value == 1))]);
            }
            _ => purifier.swing_mode = None,
        }
        self.name_main(
            purifier.name.as_mut(),
            purifier
                .configured_name
                .as_mut()
                .ok_or_else(|| anyhow!("no configured name"))?,
        )
        .await?;

        let mut services: Vec<Box<dyn HapService>> = Vec::new();
        services.extend(self.air().await?);
        services.extend(self.humidity().await?);
        services.extend(self.climate().await?);
        services.extend(self.filter().await?);
        let switches = self
            .linked_switches(aid, &[Role::Auto, Role::Oscillation, Role::Heat])
            .await?;
        purifier.set_linked_services(switches.iter().map(|switch| switch.get_id()).collect());
        let info = self.information(aid, &self.device.name.clone(), "", true)?;
        let mut all: Vec<Box<dyn HapService>> = vec![Box::new(info), Box::new(purifier)];
        all.extend(services);
        all.extend(switches);
        self.accessory(aid, all, HapType::AirPurifier, None);
        Ok(())
    }

    /// Air quality from what the device measures, on Home's five steps.
    async fn air(&mut self) -> Result<Option<Box<dyn HapService>>> {
        let measured: Vec<Capability> = [
            Role::Pm25,
            Role::Pm10,
            Role::Voc,
            Role::No2,
            Role::Formaldehyde,
        ]
        .into_iter()
        .filter_map(|role| self.role(role))
        .collect();
        if measured.is_empty() {
            return Ok(None);
        }
        let aid = self.aid("main");
        let mut air = AirQualitySensorService::new(self.iid("main", "air"), aid);
        air.nitrogen_dioxide_density = None;
        air.ozone_density = None;
        air.sulphur_dioxide_density = None;
        air.volatile_organic_compound_density = None;
        air.status_fault = None;
        air.status_low_battery = None;
        air.status_tampered = None;
        fixed(air.name.as_mut(), json!("Air quality")).await?;
        let roles: Vec<(Role, &'static str)> = measured
            .iter()
            .map(|capability| (capability.role, capability.id))
            .collect();
        let all = roles.clone();
        self.wiring
            .read(aid, &air.current_air_quality, move |snapshot| {
                let level = all
                    .iter()
                    .filter(|(_, id)| active(snapshot, id))
                    .filter_map(|(role, id)| Some(band(*role, number(snapshot, id)?)))
                    .max()
                    .unwrap_or(0);
                Some(json!(level))
            });
        let held = |role: Role| {
            roles
                .iter()
                .find(|(held, _)| *held == role)
                .map(|(_, id)| *id)
        };
        if let (Some(id), Some(density)) = (held(Role::Pm25), air.pm2_5_density.as_mut()) {
            self.wiring.read(aid, density, move |snapshot| {
                number(snapshot, id).map(|value| json!(value))
            });
        }
        if let (Some(id), Some(density)) = (held(Role::Pm10), air.pm10_density.as_mut()) {
            self.wiring.read(aid, density, move |snapshot| {
                number(snapshot, id).map(|value| json!(value))
            });
        }
        if roles.iter().all(|(role, _)| *role != Role::Pm25) {
            air.pm2_5_density = None;
        }
        if roles.iter().all(|(role, _)| *role != Role::Pm10) {
            air.pm10_density = None;
        }
        let particles: Vec<&'static str> = roles
            .iter()
            .filter(|(role, _)| matches!(role, Role::Pm25 | Role::Pm10))
            .map(|(_, id)| *id)
            .collect();
        if let Some(status) = air.status_active.as_mut() {
            self.wiring.read(aid, status, move |snapshot| {
                Some(json!(particles.iter().any(|id| active(snapshot, id))))
            });
        }
        Ok(Some(Box::new(air)))
    }

    async fn humidity(&mut self) -> Result<Option<Box<dyn HapService>>> {
        let Some(humidity) = self.role(Role::Humidity) else {
            return Ok(None);
        };
        let aid = self.aid("main");
        let mut sensor = HumiditySensorService::new(self.iid("main", "humidity"), aid);
        sensor.status_fault = None;
        sensor.status_low_battery = None;
        sensor.status_tampered = None;
        fixed(sensor.name.as_mut(), json!("Humidity")).await?;
        let id = humidity.id;
        self.wiring
            .read(aid, &sensor.current_relative_humidity, move |snapshot| {
                number(snapshot, id).map(|value| json!(value))
            });
        if let Some(status) = sensor.status_active.as_mut() {
            self.wiring.read(aid, status, move |snapshot| {
                Some(json!(active(snapshot, id)))
            });
        }
        Ok(Some(Box::new(sensor)))
    }

    /// A heater where the device heats, else the temperature it measures.
    async fn climate(&mut self) -> Result<Option<Box<dyn HapService>>> {
        let temperature = self.role(Role::Temperature).map(|capability| capability.id);
        let aid = self.aid("main");
        if let Some(heat) = self.role(Role::Heat) {
            let mut heater = HeaterCoolerService::new(self.iid("main", "climate"), aid);
            heater.lock_physical_controls = None;
            heater.rotation_speed = None;
            heater.swing_mode = None;
            heater.cooling_threshold_temperature = None;
            fixed(heater.name.as_mut(), json!("Heater")).await?;
            let id = heat.id;
            self.wiring.read(aid, &heater.active, move |snapshot| {
                Some(flag(on(snapshot, id)))
            });
            self.wiring.write(&mut heater.active, move |value: u8| {
                vec![(id, json!(value == 1))]
            });
            // It only heats. Offering Auto or Cool would read as heat off.
            HapCharacteristic::set_valid_values(
                &mut heater.target_heater_cooler_state,
                Some(vec![json!(1)]),
            )
            .map_err(|error| anyhow!("{error}"))?;
            fixed(Some(&mut heater.target_heater_cooler_state), json!(1)).await?;
            self.wiring
                .write(&mut heater.target_heater_cooler_state, move |value: u8| {
                    vec![(id, json!(value == 1))]
                });
            let target = self.role(Role::HeatTarget);
            if let (Some(target), Some(threshold)) = (
                target.clone(),
                heater.heating_threshold_temperature.as_mut(),
            ) {
                let Kind::Level { min, max, step, .. } = target.kind else {
                    unreachable!("a target is a level")
                };
                limit(threshold, Some(min), Some(max), Some(step))?;
                self.wiring.read(aid, threshold, move |snapshot| {
                    number(snapshot, target.id)
                        .map(|value| json!(value.clamp(f64::from(min), f64::from(max))))
                });
                self.wiring
                    .write(threshold, move |value: f32| vec![(target.id, json!(value))]);
            }
            if let Some(temperature) = temperature {
                self.wiring
                    .read(aid, &heater.current_temperature, move |snapshot| {
                        number(snapshot, temperature).map(|value| json!(value))
                    });
            }
            // Heating while on and short of the target; idle once there.
            let target = target.map(|target| target.id);
            self.wiring
                .read(aid, &heater.current_heater_cooler_state, move |snapshot| {
                    if !on(snapshot, id) {
                        return Some(json!(0));
                    }
                    let short = match (
                        temperature.and_then(|t| number(snapshot, t)),
                        target.and_then(|t| number(snapshot, t)),
                    ) {
                        (Some(now), Some(target)) => now < target,
                        _ => true,
                    };
                    Some(json!(if short { 2 } else { 1 }))
                });
            return Ok(Some(Box::new(heater)));
        }
        let Some(temperature) = temperature else {
            return Ok(None);
        };
        let mut sensor = TemperatureSensorService::new(self.iid("main", "climate"), aid);
        sensor.status_fault = None;
        sensor.status_low_battery = None;
        sensor.status_tampered = None;
        fixed(sensor.name.as_mut(), json!("Temperature")).await?;
        self.wiring
            .read(aid, &sensor.current_temperature, move |snapshot| {
                number(snapshot, temperature).map(|value| json!(value))
            });
        if let Some(status) = sensor.status_active.as_mut() {
            self.wiring.read(aid, status, move |snapshot| {
                Some(json!(active(snapshot, temperature)))
            });
        }
        Ok(Some(Box::new(sensor)))
    }

    async fn filter(&mut self) -> Result<Option<Box<dyn HapService>>> {
        let Some(life) = self.role(Role::FilterLife) else {
            return Ok(None);
        };
        let aid = self.aid("main");
        let mut filter = FilterMaintenanceService::new(self.iid("main", "filter"), aid);
        filter.filter_reset_change_indication = None;
        fixed(filter.name.as_mut(), json!("Filter")).await?;
        let id = life.id;
        if let Some(level) = filter.filter_life_level.as_mut() {
            self.wiring.read(aid, level, move |snapshot| {
                number(snapshot, id).map(|value| json!(value))
            });
        }
        self.wiring
            .read(aid, &filter.filter_change_indication, move |snapshot| {
                number(snapshot, id).map(|life| flag(life < 15.0))
            });
        Ok(Some(Box::new(filter)))
    }

    /// Anything else: its run or power as the tile, its switches inside it,
    /// what it uses up as maintenance, and the sensors Home notifies for as
    /// tiles of their own.
    async fn machine(&mut self) -> Result<()> {
        let aid = self.aid("main");
        let main = if self
            .device
            .capabilities
            .iter()
            .any(|capability| matches!(capability.kind, Kind::Run { .. }))
        {
            "run"
        } else {
            "power"
        };
        let mut tile = SwitchService::new(self.iid("main", main), aid);
        tile.set_primary(true);
        self.wiring.read(aid, &tile.power_state, move |snapshot| {
            Some(json!(on(snapshot, main)))
        });
        self.wiring
            .write(&mut tile.power_state, move |on| vec![(main, json!(on))]);
        self.name_main(
            tile.name.as_mut(),
            tile.configured_name
                .as_mut()
                .ok_or_else(|| anyhow!("no configured name"))?,
        )
        .await?;

        let mut services = self.linked_switches(aid, &[]).await?;
        if let Some(programme) = self.role(Role::Programme) {
            for switch in self
                .option_switches(aid, &programme, "programme", vec![("run", json!(true))])
                .await?
            {
                services.push(Box::new(switch));
            }
        }
        for supply in self
            .device
            .capabilities
            .clone()
            .into_iter()
            .filter(|capability| capability.role == Role::Supply)
        {
            let mut filter = FilterMaintenanceService::new(
                self.iid("main", &format!("supply:{}", supply.id)),
                aid,
            );
            filter.filter_life_level = None;
            filter.filter_reset_change_indication = None;
            fixed(
                filter.name.as_mut(),
                json!(supply.name.trim_end_matches(" low")),
            )
            .await?;
            let id = supply.id;
            self.wiring
                .read(aid, &filter.filter_change_indication, move |snapshot| {
                    Some(flag(on(snapshot, id)))
                });
            services.push(Box::new(filter));
        }
        tile.set_linked_services(services.iter().map(|service| service.get_id()).collect());
        let info = self.information_named(aid, &self.device.name.clone(), "", true, true)?;
        let mut all: Vec<Box<dyn HapService>> = vec![Box::new(info), Box::new(tile)];
        all.extend(services);
        self.accessory(aid, all, HapType::Switch, None);

        for capability in self.device.capabilities.clone() {
            match capability.role {
                Role::Contact => {
                    self.sensor_tile(&capability, "door", Sensor::Contact)
                        .await?
                }
                Role::Leak => self.sensor_tile(&capability, "leak", Sensor::Leak).await?,
                _ => {}
            }
            if let Kind::Run { .. } = capability.kind {
                self.sensor_tile(&capability, "cycle", Sensor::Finished)
                    .await?;
            }
        }
        Ok(())
    }

    /// A sensor as a tile of its own, which Home will notify for.
    async fn sensor_tile(
        &mut self,
        capability: &Capability,
        key: &str,
        sensor: Sensor,
    ) -> Result<()> {
        let aid = self.aid(key);
        let label = format!("{} {}", self.device.name, sensor.word(capability));
        let iid = self.iid(key, "sensor");
        let id = capability.id;
        let suffix = sensor.word(capability).to_owned();
        let service: Box<dyn HapService> = match sensor {
            Sensor::Contact => {
                let mut contact = ContactSensorService::new(iid, aid);
                contact.set_primary(true);
                quiet_status(
                    &mut contact.status_fault,
                    &mut contact.status_low_battery,
                    &mut contact.status_tampered,
                );
                fixed(contact.name.as_mut(), json!(label)).await?;
                fixed(contact.configured_name.as_mut(), json!(label)).await?;
                self.wiring
                    .read(aid, &contact.contact_sensor_state, move |snapshot| {
                        Some(flag(on(snapshot, id)))
                    });
                if let Some(status) = contact.status_active.as_mut() {
                    self.wiring
                        .read(aid, status, move |snapshot| Some(json!(snapshot.online)));
                }
                Box::new(contact)
            }
            Sensor::Leak => {
                let mut leak = LeakSensorService::new(iid, aid);
                leak.set_primary(true);
                quiet_status(
                    &mut leak.status_fault,
                    &mut leak.status_low_battery,
                    &mut leak.status_tampered,
                );
                fixed(leak.name.as_mut(), json!(label)).await?;
                fixed(leak.configured_name.as_mut(), json!(label)).await?;
                self.wiring.read(aid, &leak.leak_detected, move |snapshot| {
                    Some(flag(on(snapshot, id)))
                });
                if let Some(status) = leak.status_active.as_mut() {
                    self.wiring
                        .read(aid, status, move |snapshot| Some(json!(snapshot.online)));
                }
                Box::new(leak)
            }
            Sensor::Finished => {
                let mut motion = MotionSensorService::new(iid, aid);
                motion.set_primary(true);
                quiet_status(
                    &mut motion.status_fault,
                    &mut motion.status_low_battery,
                    &mut motion.status_tampered,
                );
                fixed(motion.name.as_mut(), json!(label)).await?;
                fixed(motion.configured_name.as_mut(), json!(label)).await?;
                // "Detected" for as long as the last run stays finished, so
                // Home notifies once per cycle.
                self.wiring
                    .read(aid, &motion.motion_detected, move |snapshot| {
                        let finished = snapshot.capabilities.iter().any(|capability| {
                            capability.id == id
                                && matches!(capability.kind, Kind::Run { finished: true, .. })
                        });
                        Some(json!(finished))
                    });
                if let Some(status) = motion.status_active.as_mut() {
                    self.wiring
                        .read(aid, status, move |snapshot| Some(json!(snapshot.online)));
                }
                Box::new(motion)
            }
        };
        let info = self.information(aid, &label, key, false)?;
        let main = service.get_type();
        self.accessory(aid, vec![Box::new(info), service], main, Some(suffix));
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Sensor {
    Contact,
    Leak,
    Finished,
}

impl Sensor {
    /// What the tile adds to the device's name.
    fn word(self, capability: &Capability) -> &str {
        match self {
            Self::Contact => "door",
            Self::Leak => capability.name.as_str(),
            Self::Finished => "cycle",
        }
    }
}

fn quiet_status<A, B, C>(fault: &mut Option<A>, battery: &mut Option<B>, tampered: &mut Option<C>) {
    *fault = None;
    *battery = None;
    *tampered = None;
}

/// A reading on Home's five steps of air quality: 1 excellent … 5 poor.
fn band(role: Role, value: f64) -> u8 {
    let edges: [f64; 4] = match role {
        // US EPA's breakpoints for PM2.5; Europe's for PM10.
        Role::Pm25 => [12.0, 35.4, 55.4, 150.0],
        Role::Pm10 => [20.0, 50.0, 100.0, 150.0],
        // An index from 0 to 10.
        Role::Voc | Role::No2 => [2.0, 4.0, 6.0, 8.0],
        // WHO's 30-minute guideline for formaldehyde is 0.1 mg/m³.
        Role::Formaldehyde => [0.03, 0.08, 0.10, 0.30],
        _ => return 0,
    };
    1 + edges.iter().filter(|edge| value > **edge).count() as u8
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
