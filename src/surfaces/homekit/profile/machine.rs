//! Anything else: its run or power as the tile, with its switches,
//! programmes and supplies inside it.

use super::*;

impl Maker<'_> {
    /// Anything else: its run or power as the tile, its switches inside it,
    /// what it uses up as maintenance, and the sensors Home notifies for as
    /// tiles of their own.
    pub(super) async fn machine(&mut self) -> Result<()> {
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
        self.notifying_tiles().await
    }
}
