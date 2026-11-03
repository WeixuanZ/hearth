//! A light: on and off, and how bright.

use super::*;

impl Maker<'_> {
    /// A light: on and off, and how bright, where it says, as Home's 0 to
    /// 100 over its own range.
    pub(super) async fn light(&mut self) -> Result<()> {
        let aid = self.aid("main");
        let mut light = LightbulbService::new(self.iid("main", "light"), aid);
        light.set_primary(true);
        light.characteristic_value_active_transition_count = None;
        light.characteristic_value_transition_control = None;
        light.supported_characteristic_value_transition_configuration = None;
        light.color_temperature = None;
        light.hue = None;
        light.saturation = None;
        self.wiring.read(aid, &light.power_state, |snapshot| {
            Some(json!(on(snapshot, "power")))
        });
        self.wiring.write(&mut light.power_state, |on: bool| {
            vec![("power", json!(on))]
        });
        let level = self.level(Role::Brightness);
        match (level, light.brightness.as_mut()) {
            (Some((level, min, max, _)), Some(brightness)) => {
                let (id, span) = (level.id, (max - min).max(f32::EPSILON));
                self.wiring.read(aid, brightness, move |snapshot| {
                    number(snapshot, id)
                        .map(|value| json!(((value as f32 - min) / span * 100.0).round() as i32))
                });
                self.wiring.write(brightness, move |percent: i32| {
                    vec![(id, json!(min + percent as f32 / 100.0 * span))]
                });
            }
            _ => light.brightness = None,
        }
        fixed(light.name.as_mut(), json!(self.device.name)).await?;
        let switches = self.linked_switches(aid, &[Role::Brightness], true).await?;
        light.set_linked_services(switches.iter().map(|switch| switch.get_id()).collect());
        let info = self.information_named(aid, &self.device.name.clone(), "", true, true)?;
        let mut all: Vec<Box<dyn HapService>> = vec![Box::new(info), Box::new(light)];
        all.extend(switches);
        // A light has no Configured Name of its own: renames go on the
        // accessory's information.
        self.accessory(aid, all, HapType::AccessoryInformation, None);
        self.notifying_tiles().await
    }
}
