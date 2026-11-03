//! Anything else. A machine that runs programmes, like a dishwasher, is a
//! valve: on starts it, off stops it, and Home counts down the time left;
//! a switch for each programme inside it starts that one. Anything else is
//! its power as a switch, with its other switches inside it.

use super::*;

impl Maker<'_> {
    /// Anything else: its run or power as the tile, its switches inside it,
    /// what it uses up as maintenance, and the sensors Home notifies for as
    /// tiles of their own.
    pub(super) async fn machine(&mut self) -> Result<()> {
        let runs = self
            .device
            .capabilities
            .iter()
            .any(|capability| matches!(capability.kind, Kind::Run { .. }));
        if runs {
            return self.appliance().await;
        }
        let aid = self.aid("main");
        let main = "power";
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

        let mut services = self.linked_switches(aid, &[], true).await?;
        services.extend(self.supplies(aid).await?);
        tile.set_linked_services(services.iter().map(|service| service.get_id()).collect());
        let info = self.information_named(aid, &self.device.name.clone(), "", true, true)?;
        let mut all: Vec<Box<dyn HapService>> = vec![Box::new(info), Box::new(tile)];
        all.extend(services);
        self.accessory(aid, all, HapType::Switch, None);
        self.notifying_tiles().await
    }

    /// A machine that runs programmes, as a valve. Its options are settings
    /// for before a run, left to the menu bar: Home has nothing for them
    /// but more switches.
    async fn appliance(&mut self) -> Result<()> {
        let aid = self.aid("main");
        let mut valve = ValveService::new(self.iid("main", "valve"), aid);
        valve.set_primary(true);
        valve.is_configured = None;
        valve.label_index = None;
        valve.set_duration = None;
        valve.status_fault = None;
        fixed(valve.name.as_mut(), json!(self.device.name)).await?;
        // A generic valve: Home shows it as on or off, and the time left.
        fixed(Some(&mut valve.valve_type), json!(0u8)).await?;
        self.wiring.read(aid, &valve.active, |snapshot| {
            Some(flag(on(snapshot, "run")))
        });
        self.wiring.write(&mut valve.active, |value: u8| {
            vec![("run", json!(value == 1))]
        });
        self.wiring.read(aid, &valve.in_use, |snapshot| {
            Some(flag(on(snapshot, "run")))
        });
        if let Some(left) = valve.remaining_duration.as_mut() {
            // Home's default stops at an hour; a programme runs longer.
            HapCharacteristic::set_max_value(left, Some(json!(86_400u32)))
                .map_err(|error| anyhow!("{error}"))?;
            self.wiring.read(aid, left, |snapshot| {
                let remaining =
                    snapshot
                        .capabilities
                        .iter()
                        .find_map(|capability| match capability.kind {
                            Kind::Run { remaining, .. } => Some(remaining.unwrap_or(0)),
                            _ => None,
                        });
                Some(json!(remaining.unwrap_or(0)))
            });
        }

        let mut services: Vec<Box<dyn HapService>> = Vec::new();
        if let Some(programme) = self.role(Role::Programme) {
            for switch in self
                .option_switches(aid, &programme, "programme", vec![("run", json!(true))])
                .await?
            {
                services.push(Box::new(switch));
            }
        }
        services.extend(self.supplies(aid).await?);
        valve.set_linked_services(services.iter().map(|service| service.get_id()).collect());
        let info = self.information_named(aid, &self.device.name.clone(), "", true, true)?;
        let mut all: Vec<Box<dyn HapService>> = vec![Box::new(info), Box::new(valve)];
        all.extend(services);
        // A valve has no Configured Name of its own: renames go on the
        // accessory's information.
        self.accessory(aid, all, HapType::AccessoryInformation, None);
        self.notifying_tiles().await
    }

    /// What it uses up, like salt, as maintenance Home warns about.
    async fn supplies(&mut self, aid: u64) -> Result<Vec<Box<dyn HapService>>> {
        let mut services: Vec<Box<dyn HapService>> = Vec::new();
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
        Ok(services)
    }
}
