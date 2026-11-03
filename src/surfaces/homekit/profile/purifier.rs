//! A purifier: the fan, its speed, swing and auto, and what it measures.

use super::*;

impl Maker<'_> {
    pub(super) async fn purifier(&mut self) -> Result<()> {
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
        match (self.level(Role::FanSpeed), purifier.rotation_speed.as_mut()) {
            (Some((speed, _, max, step)), Some(rotation)) => {
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
            .linked_switches(aid, &[Role::Auto, Role::Oscillation, Role::Heat], false)
            .await?;
        purifier.set_linked_services(switches.iter().map(|switch| switch.get_id()).collect());
        let info = self.information(aid, &self.device.name.clone(), "", true)?;
        let mut all: Vec<Box<dyn HapService>> = vec![Box::new(info), Box::new(purifier)];
        all.extend(services);
        all.extend(switches);
        self.accessory(aid, all, HapType::AirPurifier, None);
        Ok(())
    }
}
