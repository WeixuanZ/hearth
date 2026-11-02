//! A Samsung The Frame: one task that holds the TV's connections and state,
//! and shows it to HomeKit (a television and an art tile) and as capabilities.

mod capabilities;
mod settings;

pub use settings::FrameDevice;
mod link;

use crate::connectors::samsung::protocol::{self, ArtReply, Input};
use anyhow::{anyhow, Result};
use link::{Link, Network};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum Command {
    Active(bool),
    Art(bool),
    Input(u32),
    Brightness(i32),
    /// A Samsung key, from the menu bar's or the CLI's remote.
    Remote(&'static str),
}

/// Samsung's The Frame televisions.
pub struct Frame;

impl crate::driver::Driver for Frame {
    const KIND: &'static str = "frame";
    const NAME: &'static str = "Samsung The Frame";
    const CATEGORY: &'static str = "tv";
    type Settings = FrameDevice;
    type Setup = crate::connectors::samsung::Setup;

    fn setup() -> (Self::Setup, crate::setup::Step) {
        Self::Setup::start()
    }

    async fn start(settings: FrameDevice, context: crate::driver::Context) -> Result<()> {
        spawn(settings, context).await
    }

    /// The TV with its speaker and inputs, and art mode as a tile of its own.
    fn homekit_ids(tv: &FrameDevice) -> crate::surfaces::homekit::ids::DeviceIds {
        let ids = crate::surfaces::homekit::ids::DeviceIds::default()
            .accessory("main", tv.aid)
            .accessory("art", tv.art_aid)
            .service("main", "television", 20)
            .service("main", "speaker", 40)
            .service("art", "switch", 20);
        Input::ALL
            .iter()
            .zip((50..).step_by(10))
            .fold(ids, |ids, (input, iid)| {
                ids.service(
                    "main",
                    format!("input:{}", input.source().to_lowercase()),
                    iid,
                )
            })
    }
}

async fn spawn(device: FrameDevice, context: crate::driver::Context) -> Result<()> {
    let tv = Tv::new(Network::new(&device, context.clone()), device);
    context.publish(tv.snapshot());
    let requests = context.requests();
    tokio::spawn(async move {
        if let Err(error) = tv.run(requests).await {
            log::error!("Frame stopped: {error:#}");
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

    /// Takes requests from every surface, Home's included, until they end.
    async fn run(mut self, mut requests: mpsc::Receiver<crate::capabilities::Set>) -> Result<()> {
        // Look before acting: a command handled on the defaults (off, art
        // unknown) would power on or click a TV that is already showing.
        let every = Duration::from_secs(20);
        self.poll().await;
        let mut poll = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            crate::capabilities::publish(self.snapshot());
            tokio::select! {
                request = requests.recv() => {
                    let Some(request) = request else { break };
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
    struct Fake(std::sync::Arc<Mutex<Screen>>);

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

    /// Runs the TV's task over `asks`, as any surface asks, until they end.
    async fn run(fake: &Fake, asks: &[(&str, serde_json::Value)]) {
        let device = FrameDevice {
            token: "token".into(),
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel(8);
        for (capability, value) in asks {
            tx.send(crate::capabilities::Set::test(capability, value.clone()))
                .await
                .unwrap();
        }
        drop(tx);
        Tv::new(fake.clone(), device).run(rx).await.unwrap();
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
        run(&fake, &[("input", serde_json::json!("hdmi2"))]).await;
        assert_eq!(fake.sent(), [Input::Hdmi2.key()]);
        assert_eq!(fake.state(), (true, false));
    }

    #[tokio::test]
    async fn an_input_leaves_art_first_when_the_tv_says_it_is_in_art() {
        let fake = Fake::showing(true, true);
        fake.0.lock().unwrap().ip = true;
        run(&fake, &[("input", serde_json::json!("hdmi2"))]).await;
        assert_eq!(fake.sent(), ["input HDMI2"]);
        assert_eq!(fake.state(), (true, false));
    }

    #[tokio::test]
    async fn art_mode_is_not_toggled_on_a_guess() {
        let fake = Fake::showing(true, false);
        run(
            &fake,
            &[
                ("art", serde_json::json!(false)),
                ("art", serde_json::json!(true)),
            ],
        )
        .await;
        assert_eq!(fake.sent(), Vec::<String>::new());
        assert_eq!(fake.state(), (true, true));
    }

    #[tokio::test]
    async fn art_from_standby_wakes_the_tv_then_clicks_into_art() {
        let fake = Fake::default();
        run(&fake, &[("art", serde_json::json!(true))]).await;
        assert_eq!(fake.sent(), ["KEY_POWER", "KEY_POWER"]);
        assert_eq!(fake.state(), (true, true));
    }

    #[tokio::test]
    async fn turning_off_holds_the_power_key() {
        let fake = Fake::showing(true, true);
        run(&fake, &[("power", serde_json::json!(false))]).await;
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
            art.map(|c| c.value.clone())
        };
        assert_eq!(art(&tv), Some(false.into()));
        tv.on = true;
        assert_eq!(art(&tv), Some(true.into()));
    }

    fn paired() -> FrameDevice {
        FrameDevice {
            aid: 2,
            art_aid: 4,
            name: "Television".into(),
            token: "token".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn home_keeps_the_ids_it_knows_the_tv_by() {
        use crate::{driver::Driver, surfaces::homekit::Home};
        use hap::HapType;
        let device = paired();
        let snapshot = Tv::new(Fake::default(), device.clone()).snapshot();
        let home = Home::of(&snapshot, Frame::homekit_ids(&device)).await;
        assert_eq!(home.services(2, HapType::Television), [20]);
        assert_eq!(home.services(2, HapType::Speaker), [40]);
        assert_eq!(home.services(2, HapType::InputSource), [50, 60, 70, 80, 90]);
        assert_eq!(home.services(4, HapType::Switch), [20]);
    }

    #[tokio::test]
    async fn an_input_from_a_scene_leaves_the_tv_off() {
        use crate::{driver::Driver, surfaces::homekit::Home};
        use hap::HapType;
        use serde_json::json;
        let device = paired();
        let mut tv = Tv::new(Fake::default(), device.clone());
        let mut home = Home::of(&tv.snapshot(), Frame::homekit_ids(&device)).await;
        // A scene that turns the TV off may pick an input too: that asks nothing.
        let hdmi2 = json!(Input::Hdmi2 as u32);
        assert!(home
            .write(2, 20, HapType::ActiveIdentifier, hdmi2.clone())
            .await
            .is_empty());
        tv.on = true;
        home.sees(&tv.snapshot());
        assert_eq!(
            home.write(2, 20, HapType::ActiveIdentifier, hdmi2).await,
            [("input", json!("hdmi2"))]
        );
    }
}
