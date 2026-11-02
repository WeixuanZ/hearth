//! The kinds of device hearth knows. Each is a [`Driver`] in its device's
//! module, listed once in [`DRIVERS`]: its settings, how it is added, and how
//! its task starts. Nothing else names a brand.

use crate::{
    capabilities::{self, Set, Snapshot},
    config::{self, Device, Settings},
    setup::{Answers, Next, Product, Step},
    surfaces::homekit::ids::DeviceIds,
};
use anyhow::{anyhow, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::{any::Any, fmt::Debug, future::Future, marker::PhantomData, pin::Pin};
use tokio::sync::mpsc;

/// A kind of device.
pub trait Driver: Send + Sync + 'static {
    /// The config's `kind`, like `dyson`. Also how `hearth add` names it.
    const KIND: &'static str;
    /// How the menu bar offers it, like "Dyson purifier".
    const NAME: &'static str;
    /// What it is, like `purifier`. Picks its icon.
    const CATEGORY: &'static str;
    /// What the config keeps of one.
    type Settings: Settings
        + Clone
        + PartialEq
        + Debug
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + 'static;
    /// Adding one: questions, pages to open, waiting on the device.
    type Setup: Setup;

    fn setup() -> (Self::Setup, Step);

    /// Starts one's task, which runs as long as the bridge serves Home.
    fn start(settings: Self::Settings, context: Context)
        -> impl Future<Output = Result<()>> + Send;

    /// The ids Home knew one by before HomeKit was made from capabilities,
    /// so it keeps them. A kind that came after has none.
    fn homekit_ids(_settings: &Self::Settings) -> DeviceIds {
        DeviceIds::default()
    }
}

/// One device being added, a step at a time.
pub trait Setup: Send + 'static {
    fn next(&mut self, answers: &Answers) -> impl Future<Output = Result<Next>> + Send;
}

/// What a device's task has of the rest of hearth.
#[derive(Clone)]
pub struct Context {
    id: String,
}

impl Context {
    pub fn new(id: String) -> Self {
        Self { id }
    }

    /// The device as it now is, for every surface.
    pub fn publish(&self, snapshot: Snapshot) {
        capabilities::publish(snapshot);
    }

    /// Requests for it from every surface. Taken after its first snapshot.
    pub fn requests(&self) -> mpsc::Receiver<Set> {
        let (requests, received) = mpsc::channel(16);
        capabilities::listen(&self.id, requests);
        received
    }

    /// Keeps what it learned, like a new token, in its saved settings, and
    /// leaves the rest of them as they are.
    pub fn remember<S: 'static>(&self, change: impl FnOnce(&mut S)) {
        let result = config::load().and_then(|mut config| {
            let device = config
                .devices
                .iter_mut()
                .find(|device| device.id() == self.id)
                .ok_or_else(|| anyhow!("{} is no longer in the config", self.id))?;
            change(
                device
                    .get_mut::<S>()
                    .ok_or_else(|| anyhow!("{} is not that kind", self.id))?,
            );
            config::save(&config)
        });
        if let Err(error) = result {
            log::warn!("could not remember what {} said: {error:#}", self.id);
        }
    }
}

/// A driver as the rest of hearth handles it, whatever its kind.
pub trait Kind: Send + Sync {
    fn kind(&self) -> &'static str;
    fn product(&self) -> Product;
    /// One device's settings from its table in the config.
    fn read(&self, table: toml::Table) -> Result<Device>;
    fn setup(&self) -> (Box<dyn AnySetup>, Step);
    fn start(
        &self,
        device: &Device,
        context: Context,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;
    fn homekit_ids(&self, device: &Device) -> DeviceIds;
}

/// [`Setup`], for a setup of any kind.
pub trait AnySetup: Send {
    fn next<'a>(
        &'a mut self,
        answers: &'a Answers,
    ) -> Pin<Box<dyn Future<Output = Result<Next>> + Send + 'a>>;
}

impl<T: Setup> AnySetup for T {
    fn next<'a>(
        &'a mut self,
        answers: &'a Answers,
    ) -> Pin<Box<dyn Future<Output = Result<Next>> + Send + 'a>> {
        Box::pin(Setup::next(self, answers))
    }
}

/// A [`Driver`] in [`DRIVERS`].
pub struct Registered<D>(PhantomData<fn() -> D>);

impl<D> Registered<D> {
    pub const NEW: Self = Self(PhantomData);
}

impl<D: Driver> Kind for Registered<D> {
    fn kind(&self) -> &'static str {
        D::KIND
    }

    fn homekit_ids(&self, device: &Device) -> DeviceIds {
        device
            .get::<D::Settings>()
            .map(D::homekit_ids)
            .unwrap_or_default()
    }

    fn product(&self) -> Product {
        Product {
            id: D::KIND,
            name: D::NAME,
            category: D::CATEGORY,
        }
    }

    fn read(&self, table: toml::Table) -> Result<Device> {
        let settings: D::Settings = toml::Value::Table(table).try_into()?;
        Ok(Device::new::<D>(settings))
    }

    fn setup(&self) -> (Box<dyn AnySetup>, Step) {
        let (setup, step) = D::setup();
        (Box::new(setup), step)
    }

    fn start(
        &self,
        device: &Device,
        context: Context,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        match device.get::<D::Settings>() {
            Some(settings) => Box::pin(D::start(settings.clone(), context)),
            None => {
                let kind = device.kind().to_owned();
                Box::pin(async move { Err(anyhow!("{kind} is not a {} device", D::KIND)) })
            }
        }
    }
}

/// Every kind of device hearth knows, in the order the menu bar offers them.
pub static DRIVERS: &[&dyn Kind] = &[
    &Registered::<crate::devices::purifier::dyson::Dyson>::NEW,
    &Registered::<crate::devices::tv::samsung_frame::Frame>::NEW,
    &Registered::<crate::devices::dishwasher::bosch::Bosch>::NEW,
];

/// The driver for `kind`, if this build has one.
pub fn find(kind: &str) -> Option<&'static dyn Kind> {
    DRIVERS.iter().copied().find(|driver| driver.kind() == kind)
}

/// A device's settings behind [`Device`], with the kind that reads them.
pub(crate) struct Typed<D: Driver>(pub D::Settings);

impl<D: Driver> Clone for Typed<D> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<D: Driver> Debug for Typed<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<D: Driver> config::Entry for Typed<D> {
    fn kind(&self) -> &str {
        D::KIND
    }
    fn settings(&self) -> &dyn Settings {
        &self.0
    }
    fn settings_mut(&mut self) -> &mut dyn Settings {
        &mut self.0
    }
    fn table(&self) -> Result<toml::Table> {
        match toml::Value::try_from(&self.0)? {
            toml::Value::Table(table) => Ok(table),
            _ => Err(anyhow!("{} settings are not a table", D::KIND)),
        }
    }
    fn clone_entry(&self) -> Box<dyn config::Entry> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        &self.0
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        &mut self.0
    }
}
