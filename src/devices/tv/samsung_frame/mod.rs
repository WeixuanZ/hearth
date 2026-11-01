//! A Samsung The Frame: one task that holds the TV's connections and state,
//! and shows it to HomeKit (a television and an art tile) and as capabilities.

mod accessory;
mod capabilities;
mod link;

use crate::{
    config::FrameDevice,
    connectors::samsung::protocol::{self, ArtReply, Input},
    surfaces::homekit::characteristics::follow_name,
};
use accessory::{apply, apply_art, build, build_art, View};
use anyhow::{anyhow, Result};
use hap::{
    server::{IpServer, Server},
    HapType,
};
use link::{Link, Network};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone)]
pub enum Command {
    Active(bool),
    Art(bool),
    Input(u32),
    Key(u8),
    Brightness(i32),
    /// Home's volume button. HAP says 0 is louder, but Home on iOS sends 0 for
    /// the down button, as tested on a Frame.
    Volume(u8),
    Mute,
    /// A Samsung key, from the menu bar's or the CLI's remote.
    Remote(&'static str),
}

pub async fn spawn(device: FrameDevice, server: IpServer) -> Result<()> {
    let (tx, rx) = mpsc::channel(32);
    let tv = Tv::new(Network::new(&device), device);
    let snapshot = tv.snapshot();
    let id = snapshot.id.clone();
    crate::capabilities::publish(snapshot);
    let (requests_tx, requests) = mpsc::channel(16);
    crate::capabilities::listen(&id, requests_tx);
    let (state_tx, mut state_rx) = watch::channel(tv.view());
    let suppress = Arc::new(AtomicBool::new(false));
    let accessory = server
        .add_accessory(build(&tv.device, &suppress, &tx).await?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    follow_name(&id, accessory.clone(), HapType::Television);
    let art = server
        .add_accessory(build_art(&tv.device, &suppress, &tx).await?)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    tokio::spawn(async move {
        let updates = async move {
            loop {
                let current = state_rx.borrow().clone();
                suppress.store(true, Ordering::SeqCst);
                let result = async {
                    apply(accessory.lock().await.as_mut(), &current).await?;
                    apply_art(art.lock().await.as_mut(), &current).await
                }
                .await;
                suppress.store(false, Ordering::SeqCst);
                if let Err(error) = result {
                    log::warn!("Frame HomeKit update failed: {error:#}");
                }
                if state_rx.changed().await.is_err() {
                    break;
                }
            }
        };
        tokio::pin!(updates);
        tokio::select! {
            result = tv.run(rx, requests, state_tx) => {
                if let Err(error) = result {
                    log::error!("Frame stopped: {error:#}");
                }
            }
            _ = &mut updates => {}
        }
    });
    Ok(())
}

/// The TV as hearth last saw it, and the link it reaches the TV by.
struct Tv<L> {
    device: FrameDevice,
    link: L,
    on: bool,
    input: u32,
    brightness: Option<i32>,
    art: Option<bool>,
    reachable: bool,
    seen: bool,
}

impl<L: Link> Tv<L> {
    fn new(link: L, device: FrameDevice) -> Self {
        Self {
            device,
            link,
            on: false,
            input: 0,
            brightness: None,
            art: None,
            reachable: false,
            seen: false,
        }
    }

    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        mut requests: mpsc::Receiver<crate::capabilities::Set>,
        states: watch::Sender<View>,
    ) -> Result<()> {
        // Look before acting: a command handled on the defaults (off, art
        // unknown) would power on or click a TV that is already showing.
        let every = Duration::from_secs(20);
        self.poll().await;
        let mut poll = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            states.send(self.view()).ok();
            crate::capabilities::publish(self.snapshot());
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    log::info!("{}: from Home: {command:?}", self.device.name);
                    // Home's input picker works only while the TV is on. With it
                    // off, an input comes from a scene or an automation, beside
                    // turning it off, and waking the TV for it would undo that.
                    if matches!(command, Command::Input(_)) && !self.on {
                        log::info!("{}: off, so the input was not set", self.device.name);
                        continue;
                    }
                    if let Err(error) = self.handle(command).await {
                        log::warn!("{}: {error:#}", self.device.name);
                    }
                }
                Some(request) = requests.recv() => {
                    let result = match capabilities::command(&request) {
                        Ok(command) => {
                            log::info!("{}: requested: {command:?}", self.device.name);
                            self.handle(command).await
                        }
                        Err(error) => Err(error),
                    };
                    request.finish(result);
                }
                _ = poll.tick() => self.poll().await,
            }
        }
        Ok(())
    }

    async fn poll(&mut self) {
        if let Err(error) = self.refresh().await {
            log::debug!("{}: {error:#}", self.device.name);
            self.reachable = false;
            self.seen = true;
            self.on = false;
        }
    }

    fn view(&self) -> View {
        View {
            on: self.on,
            art: self.on && self.art == Some(true),
            input: self.input,
            brightness: self.brightness,
        }
    }

    async fn refresh(&mut self) -> Result<()> {
        let info = self.link.info(&self.device).await?;
        self.on = info.on;
        self.reachable = true;
        self.seen = true;
        if self.device.mac.is_empty() && !info.mac.is_empty()
            || self.device.model.is_empty() && !info.model.is_empty()
        {
            self.device.mac = info.mac;
            self.device.model = info.model;
            self.remember();
        }
        if !self.on {
            // Out of standby it may come back in art or showing a source.
            self.art = None;
            return Ok(());
        }
        if self.link.ip_control() {
            match self.link.input().await {
                Ok(source) => match Input::from_source(&source) {
                    Some(input) => self.input = input as u32,
                    None => log::debug!("unknown input {source}"),
                },
                // It refuses in art mode, which keeps the last input.
                Err(error) => log::debug!("input: {error:#}"),
            }
        }
        if self.device.token.is_empty() {
            return Ok(());
        }
        // The art channel opens a few seconds after the picture. Until then
        // the TV is on and art mode is simply not known yet.
        let replies = self
            .link
            .art(&self.device, "get_artmode_status", None)
            .await
            .unwrap_or_else(|error| {
                log::debug!("art: {error:#}");
                Vec::new()
            });
        for reply in replies {
            match reply {
                ArtReply::Mode(on) => self.art = Some(on),
                ArtReply::Brightness(level) => {
                    self.brightness = Some(protocol::home_brightness(level));
                }
            }
        }
        Ok(())
    }

    async fn handle(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Active(true) => self.power_on().await?,
            Command::Active(false) => self.power_off().await?,
            Command::Input(id) => {
                let Some(input) = Input::from_id(id) else {
                    return Ok(());
                };
                if !self.on {
                    self.power_on().await?;
                }
                if self.art == Some(true) {
                    let _ = self.set_art(false).await;
                }
                if self.link.ip_control() {
                    self.link.set_input(input.source()).await?;
                } else {
                    self.click(&input.key()).await?;
                }
                self.input = id;
            }
            Command::Art(on) => {
                if on && !self.on {
                    self.power_on().await?;
                }
                if self.on {
                    self.set_art(on).await?;
                }
            }
            Command::Key(value) => {
                if let Some(key) = protocol::hap_remote_key(value) {
                    self.click(key).await?;
                }
            }
            Command::Brightness(home) => {
                let level = protocol::art_brightness(home);
                self.brightness = Some(protocol::home_brightness(level));
                let level = level.to_string();
                let replies = self
                    .link
                    .art(&self.device, "set_brightness", Some(&level))
                    .await?;
                if replies.is_empty() {
                    log::info!("the TV did not confirm the art brightness");
                }
            }
            // Volume Selector: 0 is up, 1 is down, as the HAP spec has it and
            // the Control Center remote sends.
            Command::Volume(0) => self.click("KEY_VOLUP").await?,
            Command::Volume(_) => self.click("KEY_VOLDOWN").await?,
            Command::Mute => self.click("KEY_MUTE").await?,
            Command::Remote(key) => self.click(key).await?,
        }
        Ok(())
    }

    /// From standby, the TV comes back showing a picture, not art.
    async fn power_on(&mut self) -> Result<()> {
        let by_ip = self.link.ip_control() && self.link.set_power(true).await.is_ok();
        if !by_ip {
            match self.link.info(&self.device).await {
                Ok(info) if info.on => {
                    self.on = true;
                    return Ok(());
                }
                // In network standby the remote channel still listens.
                Ok(_) => self.click("KEY_POWER").await?,
                // Fully asleep, it answers only a wake packet, which may be enough.
                Err(_) => {
                    self.link.wake(&self.device)?;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let woke = self.link.info(&self.device).await.is_ok_and(|info| info.on);
                    if !woke {
                        self.click("KEY_POWER").await?;
                    }
                }
            }
        }
        self.on = true;
        self.art = Some(false);
        Ok(())
    }

    async fn power_off(&mut self) -> Result<()> {
        let by_ip = self.link.ip_control() && self.link.set_power(false).await.is_ok();
        if !by_ip {
            // A click toggles art, and the 2024 firmware ignores KEY_POWEROFF.
            // Only a held power key, repeated as a remote does, turns it off.
            self.press("KEY_POWER", Some(Duration::from_secs(4)))
                .await?;
        }
        self.on = false;
        self.art = None;
        Ok(())
    }

    async fn set_art(&mut self, on: bool) -> Result<()> {
        let value = if on { "on" } else { "off" };
        let replies = self
            .link
            .art(&self.device, "set_artmode_status", Some(value))
            .await
            .unwrap_or_default();
        if !replies.is_empty() {
            self.art = Some(on);
            return Ok(());
        }
        // The art channel can stay silent. One power click moves a Frame
        // showing a source into art, and one click in art leaves it, so a
        // click is right only when the mode is known: on a guess it can send
        // the TV the wrong way.
        match self.art {
            Some(art) if art != on => {
                self.click("KEY_POWER").await?;
                self.art = Some(on);
                Ok(())
            }
            Some(_) => Ok(()),
            None => Err(anyhow!(
                "the TV did not say whether it is showing art, so nothing was pressed"
            )),
        }
    }

    async fn click(&mut self, key: &str) -> Result<()> {
        self.press(key, None).await
    }

    async fn press(&mut self, key: &str, hold: Option<Duration>) -> Result<()> {
        if self.device.token.is_empty() {
            anyhow::bail!("not paired. Run `hearth frame pair` while the TV is on");
        }
        let token = self.link.press(&self.device, key, hold).await?;
        if let Some(token) = token.filter(|token| *token != self.device.token) {
            self.device.token = token;
            self.remember();
        }
        Ok(())
    }

    fn remember(&self) {
        self.link.save(&self.device);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::samsung::protocol::DeviceInfo;
    use std::sync::Mutex;

    /// A Frame in memory: a picture or art while on, and every key it got.
    #[derive(Default)]
    struct Screen {
        on: bool,
        art: bool,
        /// Whether the art channel answers. A real Frame's often stays silent.
        answers: bool,
        ip: bool,
        sent: Vec<String>,
    }

    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<Screen>>);

    impl Fake {
        fn showing(art: bool, answers: bool) -> Self {
            let fake = Self::default();
            *fake.0.lock().unwrap() = Screen {
                on: true,
                art,
                answers,
                ..Default::default()
            };
            fake
        }

        fn sent(&self) -> Vec<String> {
            self.0.lock().unwrap().sent.clone()
        }

        fn state(&self) -> (bool, bool) {
            let screen = self.0.lock().unwrap();
            (screen.on, screen.art)
        }
    }

    impl Link for Fake {
        async fn info(&self, _: &FrameDevice) -> Result<DeviceInfo> {
            Ok(DeviceInfo {
                on: self.0.lock().unwrap().on,
                mac: String::new(),
                model: String::new(),
                name: "Frame".into(),
                frame: true,
            })
        }

        async fn art(
            &self,
            _: &FrameDevice,
            request: &str,
            value: Option<&str>,
        ) -> Result<Vec<ArtReply>> {
            let mut screen = self.0.lock().unwrap();
            if !screen.answers || !screen.on {
                return Ok(Vec::new());
            }
            if request == "set_artmode_status" {
                screen.art = value == Some("on");
            }
            Ok(vec![ArtReply::Mode(screen.art)])
        }

        async fn press(
            &mut self,
            _: &FrameDevice,
            key: &str,
            hold: Option<Duration>,
        ) -> Result<Option<String>> {
            let mut screen = self.0.lock().unwrap();
            match (key, hold) {
                ("KEY_POWER", Some(_)) => (screen.on, screen.art) = (false, false),
                ("KEY_POWER", None) if screen.on => screen.art = !screen.art,
                ("KEY_POWER", None) => (screen.on, screen.art) = (true, false),
                _ => {}
            }
            let held = if hold.is_some() { " held" } else { "" };
            screen.sent.push(format!("{key}{held}"));
            Ok(None)
        }

        fn wake(&self, _: &FrameDevice) -> Result<()> {
            Ok(())
        }

        fn ip_control(&self) -> bool {
            self.0.lock().unwrap().ip
        }

        async fn set_power(&self, on: bool) -> Result<()> {
            let mut screen = self.0.lock().unwrap();
            (screen.on, screen.art) = (on, false);
            screen.sent.push(format!("power {on}"));
            Ok(())
        }

        async fn input(&self) -> Result<String> {
            Ok("HDMI1".into())
        }

        async fn set_input(&self, source: &str) -> Result<()> {
            let mut screen = self.0.lock().unwrap();
            screen.art = false;
            screen.sent.push(format!("input {source}"));
            Ok(())
        }

        fn save(&self, _: &FrameDevice) {}
    }

    /// Runs the TV's task over `commands` from Home, until they run out.
    async fn run(fake: &Fake, commands: &[Command]) {
        let device = FrameDevice {
            token: "token".into(),
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel(8);
        for command in commands {
            tx.send(command.clone()).await.unwrap();
        }
        drop(tx);
        let (_requests, requests) = mpsc::channel(1);
        let tv = Tv::new(fake.clone(), device);
        let (states, _) = watch::channel(tv.view());
        tv.run(rx, requests, states).await.unwrap();
    }

    #[tokio::test]
    async fn starting_up_leaves_the_tv_alone() {
        for art in [false, true] {
            let fake = Fake::showing(art, false);
            run(&fake, &[]).await;
            assert_eq!(fake.sent(), Vec::<String>::new());
            assert_eq!(fake.state(), (true, art));
        }
    }

    #[tokio::test]
    async fn an_input_on_a_tv_already_showing_one_presses_only_its_key() {
        // The art channel is silent, so the mode is not known: a power click
        // to leave art would have sent a picture to art instead.
        let fake = Fake::showing(false, false);
        run(&fake, &[Command::Input(Input::Hdmi2 as u32)]).await;
        assert_eq!(fake.sent(), [Input::Hdmi2.key()]);
        assert_eq!(fake.state(), (true, false));
    }

    #[tokio::test]
    async fn an_input_leaves_art_first_when_the_tv_says_it_is_in_art() {
        let fake = Fake::showing(true, true);
        fake.0.lock().unwrap().ip = true;
        run(&fake, &[Command::Input(Input::Hdmi2 as u32)]).await;
        assert_eq!(fake.sent(), ["input HDMI2"]);
        assert_eq!(fake.state(), (true, false));
    }

    #[tokio::test]
    async fn art_mode_is_not_toggled_on_a_guess() {
        let fake = Fake::showing(true, false);
        run(&fake, &[Command::Art(false), Command::Art(true)]).await;
        assert_eq!(fake.sent(), Vec::<String>::new());
        assert_eq!(fake.state(), (true, true));
    }

    #[tokio::test]
    async fn art_from_standby_wakes_the_tv_then_clicks_into_art() {
        let fake = Fake::default();
        run(&fake, &[Command::Art(true)]).await;
        assert_eq!(fake.sent(), ["KEY_POWER", "KEY_POWER"]);
        assert_eq!(fake.state(), (true, true));
    }

    #[tokio::test]
    async fn a_scene_that_turns_the_tv_off_with_an_input_leaves_it_off() {
        let fake = Fake::showing(false, false);
        run(
            &fake,
            &[Command::Active(false), Command::Input(Input::Hdmi4 as u32)],
        )
        .await;
        assert_eq!(fake.sent(), ["KEY_POWER held"]);
        assert_eq!(fake.state(), (false, false));
    }

    #[tokio::test]
    async fn turning_off_holds_the_power_key() {
        let fake = Fake::showing(true, true);
        run(&fake, &[Command::Active(false)]).await;
        assert_eq!(fake.sent(), ["KEY_POWER held"]);
        assert_eq!(fake.state(), (false, false));
    }

    #[test]
    fn art_mode_shows_only_while_the_tv_is_on() {
        let mut tv = Tv::new(Fake::default(), FrameDevice::default());
        tv.art = Some(true);
        let art = |tv: &Tv<Fake>| {
            let snapshot = tv.snapshot();
            let art = snapshot.capabilities.iter().find(|c| c.id == "art");
            (tv.view().art, art.map(|c| c.value.clone()))
        };
        assert_eq!(art(&tv), (false, Some(false.into())));
        tv.on = true;
        assert_eq!(art(&tv), (true, Some(true.into())));
    }
}
