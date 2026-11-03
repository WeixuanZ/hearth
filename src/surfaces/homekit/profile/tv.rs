//! A television: its power, its inputs as sources, its remote, its speaker,
//! and each other switch as a tile of its own.

use super::*;

impl Maker<'_> {
    pub(super) async fn tv(&mut self) -> Result<()> {
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
        if let Some((input, options)) = self.choice(Role::Input) {
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
                // A tuner for the TV's own channels, HDMI by its name, the
                // home screen as Home has one; else other.
                let kind = if option.id == "tv" {
                    2
                } else if option.id.starts_with("hdmi") {
                    3
                } else if option.id == "home" {
                    1
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
}
