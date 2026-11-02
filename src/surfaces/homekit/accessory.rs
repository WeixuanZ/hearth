//! An accessory of any services, and what ties its characteristics to a
//! device: how each one reads the device's state, and what Home writing it
//! asks of the device.

use crate::capabilities::Snapshot;
use anyhow::{anyhow, Result};
use hap::{
    accessory::HapAccessory,
    characteristic::{CharacteristicCallbacks, HapCharacteristic},
    service::HapService,
    HapType,
};
use serde::ser::{Serialize, SerializeStruct, Serializer};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{mpsc, watch};

/// One accessory: the information service first, then the device's own.
pub struct Accessory {
    pub id: u64,
    pub services: Vec<Box<dyn HapService>>,
}

impl HapAccessory for Accessory {
    fn get_id(&self) -> u64 {
        self.id
    }
    fn set_id(&mut self, id: u64) {
        self.id = id;
    }
    fn get_service(&self, hap_type: HapType) -> Option<&dyn HapService> {
        self.services
            .iter()
            .find(|service| service.get_type() == hap_type)
            .map(|service| service.as_ref())
    }
    fn get_mut_service(&mut self, hap_type: HapType) -> Option<&mut dyn HapService> {
        self.services
            .iter_mut()
            .find(|service| service.get_type() == hap_type)
            .map(|service| service.as_mut() as &mut dyn HapService)
    }
    fn get_services(&self) -> Vec<&dyn HapService> {
        self.services
            .iter()
            .map(|service| service.as_ref())
            .collect()
    }
    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        self.services
            .iter_mut()
            .map(|service| service.as_mut() as &mut dyn HapService)
            .collect()
    }
}

impl Serialize for Accessory {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("HapAccessory", 2)?;
        state.serialize_field("aid", &self.id)?;
        state.serialize_field("services", &self.get_services())?;
        state.end()
    }
}

/// What Home writing a characteristic asks of the device: capability sets,
/// in order. Empty asks nothing.
pub type Asks = Vec<(&'static str, Value)>;

/// What a characteristic's value can be.
pub trait Settable:
    std::fmt::Debug + Default + Clone + serde::Serialize + Send + Sync + 'static
{
}
impl<T: std::fmt::Debug + Default + Clone + serde::Serialize + Send + Sync + 'static> Settable
    for T
{
}

/// What a characteristic shows of the device's state. None leaves it.
pub type Read = Box<dyn Fn(&Snapshot) -> Option<Value> + Send + Sync>;

/// How a characteristic follows the device's state.
pub struct Binding {
    pub aid: u64,
    pub iid: u64,
    pub read: Read,
}

/// Ties characteristics to one device as accessories are made.
pub struct Wiring {
    writes: mpsc::Sender<Asks>,
    /// Set while the bridge itself writes a value, so that is not taken for
    /// Home asking.
    pub quiet: Arc<AtomicBool>,
    pub bindings: Vec<Binding>,
    /// The device as last shown, for a write whose meaning depends on it.
    latest: watch::Sender<Arc<Snapshot>>,
}

impl Wiring {
    pub fn new(writes: mpsc::Sender<Asks>, snapshot: &Snapshot) -> Self {
        Self {
            writes,
            quiet: Arc::new(AtomicBool::new(false)),
            bindings: Vec::new(),
            latest: watch::Sender::new(Arc::new(snapshot.clone())),
        }
    }

    /// The device's state as Home now shows it.
    pub fn seen(&self, snapshot: Arc<Snapshot>) {
        self.latest.send_replace(snapshot);
    }

    /// `characteristic` shows what `read` makes of the device's state.
    pub fn read(
        &mut self,
        aid: u64,
        characteristic: &dyn HapCharacteristic,
        read: impl Fn(&Snapshot) -> Option<Value> + Send + Sync + 'static,
    ) {
        self.bindings.push(Binding {
            aid,
            iid: characteristic.get_id(),
            read: Box::new(read),
        });
    }

    /// Home writing `characteristic` asks what `ask` makes of the value.
    pub fn write<T: Settable>(
        &self,
        characteristic: &mut impl CharacteristicCallbacks<T>,
        ask: impl Fn(T) -> Asks + Send + Sync + 'static,
    ) {
        self.write_seeing(characteristic, move |value, _| ask(value));
    }

    /// Like `write`, for a value whose meaning depends on the device's
    /// state, as Home shows it when it writes.
    pub fn write_seeing<T: Settable>(
        &self,
        characteristic: &mut impl CharacteristicCallbacks<T>,
        ask: impl Fn(T, &Snapshot) -> Asks + Send + Sync + 'static,
    ) {
        let (quiet, writes) = (self.quiet.clone(), self.writes.clone());
        let latest = self.latest.subscribe();
        characteristic.on_update(Some(move |_old: &T, new: &T| {
            if !quiet.load(Ordering::SeqCst) {
                let asks = ask(new.clone(), &latest.borrow());
                if !asks.is_empty() {
                    let _ = writes.try_send(asks);
                }
            }
            Ok(())
        }));
    }
}

/// Sets a value that does not follow the device, like a name or a type.
pub async fn fixed(
    characteristic: Option<&mut impl HapCharacteristic>,
    value: Value,
) -> Result<()> {
    if let Some(characteristic) = characteristic {
        characteristic
            .set_value(value)
            .await
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(())
}

/// Shows `value` on the characteristic `iid` of `accessory`.
pub async fn show(accessory: &mut dyn HapAccessory, iid: u64, value: Value) -> Result<()> {
    for service in accessory.get_mut_services() {
        for characteristic in service.get_mut_characteristics() {
            if characteristic.get_id() == iid {
                return characteristic
                    .set_value(value)
                    .await
                    .map_err(|error| anyhow!("{error}"));
            }
        }
    }
    Ok(())
}
