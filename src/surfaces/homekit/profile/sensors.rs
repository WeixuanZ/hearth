//! What a device measures, as Home's sensors, and the sensors Home
//! notifies for, as tiles of their own.

use super::*;

impl Maker<'_> {
    /// Air quality from what the device measures, on Home's five steps.
    pub(super) async fn air(&mut self) -> Result<Option<Box<dyn HapService>>> {
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

    pub(super) async fn humidity(&mut self) -> Result<Option<Box<dyn HapService>>> {
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
    pub(super) async fn climate(&mut self) -> Result<Option<Box<dyn HapService>>> {
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
            let target = self.level(Role::HeatTarget);
            if let (Some((target, min, max, step)), Some(threshold)) = (
                target.clone(),
                heater.heating_threshold_temperature.as_mut(),
            ) {
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
            let target = target.map(|(target, ..)| target.id);
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

    pub(super) async fn filter(&mut self) -> Result<Option<Box<dyn HapService>>> {
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

    /// Something that only measures: its temperature, humidity and air
    /// quality as Home's sensors. One that measures nothing Home shows is a
    /// machine.
    pub(super) async fn sensor(&mut self) -> Result<()> {
        let aid = self.aid("main");
        let mut services: Vec<Box<dyn HapService>> = Vec::new();
        services.extend(self.climate().await?);
        services.extend(self.humidity().await?);
        services.extend(self.air().await?);
        let Some(first) = services.first_mut() else {
            return self.machine().await;
        };
        first.set_primary(true);
        let info = self.information_named(aid, &self.device.name.clone(), "", true, true)?;
        let mut all: Vec<Box<dyn HapService>> = vec![Box::new(info)];
        all.extend(services);
        self.accessory(aid, all, HapType::AccessoryInformation, None);
        self.notifying_tiles().await
    }

    /// The sensors Home notifies for, as tiles of their own: a door, a
    /// leak, a run that finished.
    pub(super) async fn notifying_tiles(&mut self) -> Result<()> {
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
