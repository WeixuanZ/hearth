//! Matter, beside HomeKit, for Google Home, Alexa, SmartThings and Home
//! Assistant: one bridge, with each device's power and switches as plugs
//! and its readings as sensors (see `aspects`). Like every surface it reads
//! the devices from the bus and sends requests through `capabilities::set`.
//!
//! Off until `hearth matter on`. rs-matter runs it on a thread of its own,
//! from when this computer serves Home until it steps down. What a
//! controller paired with, and each endpoint's id, are kept under
//! `homekit/matter`, which the group shares, so another computer that takes
//! over serves the same bridge.

mod aspects;

use crate::{
    bus,
    capabilities::{self, Snapshot},
    config::{self, Config},
};
use anyhow::Result;
use aspects::{Aspect, Bridged, Endpoints};
use core::pin::pin;
use embassy_futures::select::select4;
use rs_matter::{
    clusters,
    crypto::{default_crypto, Crypto},
    dm::{
        clusters::{
            basic_info::BasicInfoConfig,
            decl::{
                boolean_state, bridged_device_basic_information, on_off,
                relative_humidity_measurement, temperature_measurement,
            },
            desc::{self, ClusterHandler as _},
            groups::{self, ClusterHandler as _},
        },
        devices::{
            test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_DET},
            DEV_TYPE_AGGREGATOR, DEV_TYPE_BRIDGED_NODE,
        },
        endpoints,
        networks::{eth::EthNetwork, SysNetifs},
        Async, Cluster, DataModel, Dataver, DeviceType, Endpoint, HandlerContext, InvokeContext,
        Node, ReadContext,
    },
    error::{Error, ErrorCode},
    im::{EthInteractionModelState, InteractionModel},
    persist::DirKvBlobStore,
    respond::DefaultResponder,
    root_endpoint,
    sc::pase::MAX_COMM_WINDOW_TIMEOUT_SECS,
    tlv::{Nullable, TLVBuilderParent, Utf8StrBuilder},
    transport::{exchange::MatterBuffers, MATTER_SOCKET_BIND_ADDR},
    utils::select::Coalesce,
    with, BasicCommData, Matter, MATTER_PORT,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{net::UdpSocket, path::PathBuf, sync::Arc};
use tokio::sync::oneshot;

/// What the config keeps: the code a controller pairs with. Random to each
/// bridge, not the test code every rs-matter example shares.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub passcode: u32,
    pub discriminator: u16,
}

/// Passcodes the specification forbids, as too easy to guess.
const FORBIDDEN: [u32; 12] = [
    0, 11111111, 22222222, 33333333, 44444444, 55555555, 66666666, 77777777, 88888888, 99999999,
    12345678, 87654321,
];

impl Settings {
    pub fn new() -> Self {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let passcode = loop {
            let passcode = rng.gen_range(1..=99_999_998);
            if !FORBIDDEN.contains(&passcode) {
                break passcode;
            }
        };
        Self {
            passcode,
            discriminator: rng.gen_range(0..4096),
        }
    }

    fn comm(&self) -> BasicCommData {
        use rs_matter::sc::pase::{Spake2pVerifierPassword, Spake2pVerifierPasswordRef};
        BasicCommData {
            password: Spake2pVerifierPassword::new_from_ref(Spake2pVerifierPasswordRef::new(
                &self.passcode.to_le_bytes(),
            )),
            discriminator: self.discriminator,
        }
    }

    /// The manual pairing code, as a controller asks for it.
    pub fn code(&self) -> String {
        self.comm().compute_pretty_pairing_code().to_string()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::new()
    }
}

/// Matter while it serves; it stops when this is dropped.
pub struct Running {
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

fn dir() -> Result<PathBuf> {
    Ok(config::homekit_dir()?.join("matter"))
}

/// Serves Matter for the devices that have published, if it is on. Run
/// once they have started, like HomeKit's accessories.
pub fn start(config: &Config) -> Result<Option<Running>> {
    let Some(settings) = config.matter else {
        return Ok(None);
    };
    let dir = dir()?;
    let mut endpoints = Endpoints::load(&dir)?;
    let snapshots: Vec<Snapshot> = config
        .devices
        .iter()
        .filter_map(|device| capabilities::snapshot(&device.id()).ok())
        .collect();
    let bridged = aspects::bridged(&snapshots, &mut endpoints);
    endpoints.save(&dir)?;
    let tokio = tokio::runtime::Handle::current();
    let serial = config.bridge_id.clone();
    let (stop, stopped) = oneshot::channel();
    std::thread::Builder::new()
        .name("matter".into())
        .spawn(move || {
            if let Err(error) = run(settings, dir, serial, bridged, tokio, stopped) {
                log::error!("Matter stopped: {error:?}");
            }
        })?;
    log::info!(
        "serving Matter. To add it in a Matter controller, enter code {}",
        settings.code()
    );
    Ok(Some(Running { stop: Some(stop) }))
}

/// Forgets every controller Matter was paired with, as `hearth matter off`.
pub fn forget() -> Result<()> {
    match std::fs::remove_dir_all(dir()?.join("kv")) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

fn run(
    settings: Settings,
    dir: PathBuf,
    serial: String,
    bridged: Vec<Bridged>,
    tokio: tokio::runtime::Handle,
    stopped: oneshot::Receiver<()>,
) -> Result<(), Error> {
    // The test attestation, so a controller warns that it is not certified,
    // as for any device made at home.
    let info = BasicInfoConfig {
        vendor_name: "Hearth",
        product_name: "Hearth",
        device_name: "Hearth",
        serial_no: &serial,
        ..TEST_DEV_DET
    };
    let matter = Matter::new(&info, settings.comm(), &TEST_DEV_ATT, MATTER_PORT);
    let kv = matter.kv(DirKvBlobStore::new(dir.join("kv")));
    matter.startup(&kv)?;
    let buffers: MatterBuffers = MatterBuffers::new();
    let state: EthInteractionModelState = EthInteractionModelState::new(EthNetwork::new_default());
    let crypto = default_crypto(rand10::rng(), DAC_PRIVKEY);
    let mut rand = crypto.rand()?;
    let bridge = Bridge::new(bridged, tokio, &mut rand);
    let node = node(&bridge.bridged);
    let im = InteractionModel::new(
        &matter,
        &crypto,
        &buffers,
        data_model(node, rand, &bridge),
        &kv,
        &state,
    );
    futures_lite::future::block_on(im.startup())?;
    let responder = DefaultResponder::new(&im);
    let mut respond = pin!(responder.run::<4, 4>());
    let mut im_job = pin!(im.run());
    let socket = async_io::Async::<UdpSocket>::bind(MATTER_SOCKET_BIND_ADDR)?;
    let mut mdns = pin!(mdns(&matter));
    let mut transport = pin!(matter.run(&crypto, &socket, &socket, &socket));
    if !matter.has_fabrics() {
        matter.open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS, &crypto, &())?;
    }
    let all = select4(&mut transport, &mut mdns, &mut respond, &mut im_job).coalesce();
    futures_lite::future::block_on(futures_lite::future::or(all, async {
        let _ = stopped.await;
        Ok(())
    }))
}

/// Advertising, through the system's own mDNS: Bonjour on a Mac, Avahi
/// elsewhere.
async fn mdns(matter: &Matter<'_>) -> Result<(), Error> {
    #[cfg(target_os = "macos")]
    {
        rs_matter::transport::network::mdns::astro::AstroMdns::new()
            .run(matter)
            .await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let bus = rs_matter::utils::zbus::Connection::system()
            .await
            .map_err(|_| Error::from(ErrorCode::NoNetworkInterface))?;
        rs_matter::transport::network::mdns::avahi::AvahiMdns::new(bus)
            .run(matter)
            .await
    }
}

const PLUG: DeviceType = DeviceType {
    dtype: 0x010A,
    drev: 3,
};
const TEMPERATURE_SENSOR: DeviceType = DeviceType {
    dtype: 0x0302,
    drev: 2,
};
const HUMIDITY_SENSOR: DeviceType = DeviceType {
    dtype: 0x0307,
    drev: 2,
};
const CONTACT_SENSOR: DeviceType = DeviceType {
    dtype: 0x0015,
    drev: 1,
};
const LEAK_DETECTOR: DeviceType = DeviceType {
    dtype: 0x0043,
    drev: 1,
};

const INFO: Cluster<'static> = bridged_device_basic_information::FULL_CLUSTER
    .with_features(0)
    .with_attrs(with!(
        required;
        bridged_device_basic_information::AttributeId::NodeLabel
    ))
    .with_cmds(with!());
const ON_OFF: Cluster<'static> = on_off::FULL_CLUSTER
    .with_features(0)
    .with_attrs(with!(required))
    .with_cmds(with!(
        on_off::CommandId::Off | on_off::CommandId::On | on_off::CommandId::Toggle
    ));
const TEMPERATURE: Cluster<'static> = temperature_measurement::FULL_CLUSTER
    .with_attrs(with!(required))
    .with_cmds(with!());
const HUMIDITY: Cluster<'static> = relative_humidity_measurement::FULL_CLUSTER
    .with_attrs(with!(required))
    .with_cmds(with!());
const BOOLEAN: Cluster<'static> = boolean_state::FULL_CLUSTER
    .with_attrs(with!(required))
    .with_cmds(with!());

const AGGREGATOR: &[DeviceType] = &[DEV_TYPE_AGGREGATOR];
const AGGREGATOR_CLUSTERS: &[Cluster<'static>] = clusters!(desc::DescHandler::CLUSTER);

const SWITCH: &[Cluster<'static>] = clusters!(
    desc::DescHandler::CLUSTER,
    groups::GroupsHandler::CLUSTER,
    INFO,
    ON_OFF
);
const TEMPERATURE_CLUSTERS: &[Cluster<'static>] = clusters!(
    desc::DescHandler::CLUSTER,
    groups::GroupsHandler::CLUSTER,
    INFO,
    TEMPERATURE
);
const HUMIDITY_CLUSTERS: &[Cluster<'static>] = clusters!(
    desc::DescHandler::CLUSTER,
    groups::GroupsHandler::CLUSTER,
    INFO,
    HUMIDITY
);
const BOOLEAN_CLUSTERS: &[Cluster<'static>] = clusters!(
    desc::DescHandler::CLUSTER,
    groups::GroupsHandler::CLUSTER,
    INFO,
    BOOLEAN
);

/// The device types and clusters of an endpoint for `aspect`.
fn shape(aspect: Aspect) -> (&'static [DeviceType], &'static [Cluster<'static>]) {
    match aspect {
        Aspect::Switch(_) => (&[PLUG, DEV_TYPE_BRIDGED_NODE], SWITCH),
        Aspect::Temperature(_) => (
            &[TEMPERATURE_SENSOR, DEV_TYPE_BRIDGED_NODE],
            TEMPERATURE_CLUSTERS,
        ),
        Aspect::Humidity(_) => (&[HUMIDITY_SENSOR, DEV_TYPE_BRIDGED_NODE], HUMIDITY_CLUSTERS),
        Aspect::Contact(_) => (&[CONTACT_SENSOR, DEV_TYPE_BRIDGED_NODE], BOOLEAN_CLUSTERS),
        Aspect::Leak(_) => (&[LEAK_DETECTOR, DEV_TYPE_BRIDGED_NODE], BOOLEAN_CLUSTERS),
    }
}

const ROOT: Endpoint<'static> = root_endpoint!(eth);

/// The node: the root, the aggregator, and an endpoint per bridged thing.
/// Made once per serve, so its endpoints are leaked to live as long.
fn node(bridged: &[Bridged]) -> Node<'static> {
    let mut endpoints = vec![ROOT, Endpoint::new(1, AGGREGATOR, AGGREGATOR_CLUSTERS)];
    for one in bridged {
        let (types, clusters) = shape(one.aspect);
        endpoints.push(Endpoint::new(one.endpoint, types, clusters));
    }
    Node {
        endpoints: endpoints.leak(),
    }
}

fn data_model<'a>(
    node: Node<'static>,
    mut rand: impl rand10::Rng + Copy,
    bridge: &'a Bridge,
) -> impl DataModel + 'a {
    (
        node,
        endpoints::EthSysHandlerBuilder::new()
            .netif_diag(&SysNetifs)
            .build(rand)
            .chain(
                |e, c| e == 1 && c == desc::DescHandler::CLUSTER.id,
                Async(desc::DescHandler::new_aggregator(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e >= 2 && c == desc::DescHandler::CLUSTER.id,
                Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e >= 2 && c == groups::GroupsHandler::CLUSTER.id,
                Async(groups::GroupsHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e >= 2 && c == INFO.id,
                Async(bridged_device_basic_information::HandlerAdaptor(Info(
                    bridge,
                ))),
            )
            .chain(
                |e, c| e >= 2 && c == ON_OFF.id,
                Async(on_off::HandlerAdaptor(Power(bridge))),
            )
            .chain(
                |e, c| e >= 2 && c == TEMPERATURE.id,
                Async(temperature_measurement::HandlerAdaptor(Temperature(bridge))),
            )
            .chain(
                |e, c| e >= 2 && c == HUMIDITY.id,
                Async(relative_humidity_measurement::HandlerAdaptor(Humidity(
                    bridge,
                ))),
            )
            .chain(
                |e, c| e >= 2 && c == BOOLEAN.id,
                Async(boolean_state::HandlerAdaptor(Boolean(bridge))),
            ),
    )
}

/// The bridged endpoints, read from the bus and set through capabilities.
struct Bridge {
    bridged: Vec<Bridged>,
    /// One version for every bridged cluster: any change makes a controller
    /// read again, which is cheap at a home's scale.
    version: Dataver,
    tokio: tokio::runtime::Handle,
}

impl Bridge {
    fn new(
        bridged: Vec<Bridged>,
        tokio: tokio::runtime::Handle,
        rand: &mut impl rand10::Rng,
    ) -> Self {
        Self {
            bridged,
            version: Dataver::new_rand(rand),
            tokio,
        }
    }

    fn at(&self, endpoint: u16) -> Result<&Bridged, Error> {
        self.bridged
            .iter()
            .find(|one| one.endpoint == endpoint)
            .ok_or_else(|| ErrorCode::EndpointNotFound.into())
    }

    fn device(&self, one: &Bridged) -> Option<Arc<Snapshot>> {
        bus::watch().borrow().get(&one.device).cloned()
    }

    /// The value of the endpoint's capability, null while not known.
    fn value(&self, endpoint: u16) -> Result<Value, Error> {
        let one = self.at(endpoint)?;
        let id = one.aspect.capability();
        Ok(self
            .device(one)
            .and_then(|snapshot| {
                snapshot
                    .capabilities
                    .iter()
                    .find(|capability| capability.id == id)
                    .map(|capability| capability.value.clone())
            })
            .unwrap_or(Value::Null))
    }

    /// Asks the device as any surface asks it. A controller is answered at
    /// once; the device's report then shows what it did.
    fn set(&self, endpoint: u16, on: bool) -> Result<(), Error> {
        let one = self.at(endpoint)?;
        let (device, capability) = (one.device.clone(), one.aspect.capability());
        self.tokio.spawn(async move {
            if let Err(error) = capabilities::set(&device, capability, json!(on)).await {
                log::info!("{device}: Matter asked {capability} = {on}: {error:#}");
            }
        });
        Ok(())
    }

    /// Tells controllers of each change, for as long as Matter serves.
    async fn follow(&self, ctx: &impl HandlerContext) -> Result<(), Error> {
        let mut state = bus::watch();
        let mut seen = state.borrow_and_update().clone();
        while state.changed().await.is_ok() {
            let now = state.borrow_and_update().clone();
            let changed: Vec<u16> = self
                .bridged
                .iter()
                .filter(|one| match (seen.get(&one.device), now.get(&one.device)) {
                    (Some(was), Some(is)) => !Arc::ptr_eq(was, is),
                    (None, None) => false,
                    _ => true,
                })
                .map(|one| one.endpoint)
                .collect();
            if !changed.is_empty() {
                self.version.changed();
                for endpoint in changed {
                    ctx.notify_endpoint_changed(endpoint);
                }
            }
            seen = now;
        }
        Ok(())
    }
}

struct Info<'a>(&'a Bridge);

impl bridged_device_basic_information::ClusterHandler for Info<'_> {
    const CLUSTER: Cluster<'static> = INFO;

    fn dataver(&self) -> u32 {
        self.0.version.get()
    }

    fn dataver_changed(&self) {
        self.0.version.changed();
    }

    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        self.0.follow(&ctx).await
    }

    fn reachable(&self, ctx: impl ReadContext) -> Result<bool, Error> {
        let one = self.0.at(ctx.attr().endpoint_id)?;
        Ok(self.0.device(one).is_some_and(|snapshot| snapshot.online))
    }

    fn node_label<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: Utf8StrBuilder<P>,
    ) -> Result<P, Error> {
        let one = self.0.at(ctx.attr().endpoint_id)?;
        // The device's name now, so a rename reaches the controller.
        let name = self
            .0
            .device(one)
            .map(|snapshot| snapshot.name.clone())
            .unwrap_or_default();
        builder.set(&format!("{name}{}", one.suffix))
    }

    fn handle_keep_active(
        &self,
        _ctx: impl InvokeContext,
        _request: bridged_device_basic_information::KeepActiveRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }
}

struct Power<'a>(&'a Bridge);

impl on_off::ClusterHandler for Power<'_> {
    const CLUSTER: Cluster<'static> = ON_OFF;

    fn dataver(&self) -> u32 {
        self.0.version.get()
    }

    fn dataver_changed(&self) {
        self.0.version.changed();
    }

    fn on_off(&self, ctx: impl ReadContext) -> Result<bool, Error> {
        Ok(self.0.value(ctx.attr().endpoint_id)? == json!(true))
    }

    fn handle_off(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        self.0.set(ctx.cmd().endpoint_id, false)
    }

    fn handle_on(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        self.0.set(ctx.cmd().endpoint_id, true)
    }

    fn handle_toggle(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        let endpoint = ctx.cmd().endpoint_id;
        let on = self.0.value(endpoint)? == json!(true);
        self.0.set(endpoint, !on)
    }

    fn handle_off_with_effect(
        &self,
        _ctx: impl InvokeContext,
        _request: on_off::OffWithEffectRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    fn handle_on_with_recall_global_scene(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    fn handle_on_with_timed_off(
        &self,
        _ctx: impl InvokeContext,
        _request: on_off::OnWithTimedOffRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }
}

/// A reading in hundredths, as Matter's measurement clusters give it.
fn hundredths(value: &Value) -> Option<f64> {
    value.as_f64().map(|value| (value * 100.0).round())
}

struct Temperature<'a>(&'a Bridge);

impl temperature_measurement::ClusterHandler for Temperature<'_> {
    const CLUSTER: Cluster<'static> = TEMPERATURE;

    fn dataver(&self) -> u32 {
        self.0.version.get()
    }

    fn dataver_changed(&self) {
        self.0.version.changed();
    }

    fn measured_value(&self, ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        let value = self.0.value(ctx.attr().endpoint_id)?;
        Ok(Nullable::new(hundredths(&value).map(|value| value as i16)))
    }

    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::new(None))
    }

    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::new(None))
    }
}

struct Humidity<'a>(&'a Bridge);

impl relative_humidity_measurement::ClusterHandler for Humidity<'_> {
    const CLUSTER: Cluster<'static> = HUMIDITY;

    fn dataver(&self) -> u32 {
        self.0.version.get()
    }

    fn dataver_changed(&self) {
        self.0.version.changed();
    }

    fn measured_value(&self, ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        let value = self.0.value(ctx.attr().endpoint_id)?;
        Ok(Nullable::new(
            hundredths(&value).map(|value| value.clamp(0.0, 10000.0) as u16),
        ))
    }

    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::new(None))
    }

    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::new(None))
    }
}

struct Boolean<'a>(&'a Bridge);

impl boolean_state::ClusterHandler for Boolean<'_> {
    const CLUSTER: Cluster<'static> = BOOLEAN;

    fn dataver(&self) -> u32 {
        self.0.version.get()
    }

    fn dataver_changed(&self) {
        self.0.version.changed();
    }

    /// For a contact sensor true is closed, so a door that is open is
    /// false; for a leak detector true is a leak.
    fn state_value(&self, ctx: impl ReadContext) -> Result<bool, Error> {
        let endpoint = ctx.attr().endpoint_id;
        let on = self.0.value(endpoint)? == json!(true);
        Ok(match self.0.at(endpoint)?.aspect {
            Aspect::Contact(_) => !on,
            _ => on,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_bridge_has_its_own_code() {
        let one = Settings::new();
        assert!((1..=99_999_998).contains(&one.passcode));
        assert!(!FORBIDDEN.contains(&one.passcode));
        assert!(one.discriminator < 4096);
        let code = one.code();
        assert_eq!(code.replace('-', "").len(), 11, "{code}");
        assert_ne!(Settings::new(), one);
    }

    #[test]
    fn every_aspect_has_a_shape_with_the_bridged_cluster() {
        for aspect in [
            Aspect::Switch("power"),
            Aspect::Temperature("temperature"),
            Aspect::Humidity("humidity"),
            Aspect::Contact("door"),
            Aspect::Leak("leak"),
        ] {
            let (types, clusters) = shape(aspect);
            assert!(types
                .iter()
                .any(|kind| kind.dtype == DEV_TYPE_BRIDGED_NODE.dtype));
            assert!(clusters.iter().any(|cluster| cluster.id == INFO.id));
        }
    }

    /// Starts the bridge with nothing in it, and finds it advertised for
    /// pairing, with its own discriminator. Uses port 5540 and the network.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "serves Matter on this network for a few seconds"]
    async fn a_bridge_with_no_controller_asks_to_be_paired() {
        let settings = Settings::new();
        let dir = std::env::temp_dir().join(format!("hearth-matter-{}", rand::random::<u64>()));
        let (stop, stopped) = oneshot::channel();
        let tokio = tokio::runtime::Handle::current();
        let (dir_for_run, settings_for_run) = (dir.clone(), settings);
        let running = std::thread::spawn(move || {
            run(
                settings_for_run,
                dir_for_run,
                "hearth-test".into(),
                Vec::new(),
                tokio,
                stopped,
            )
        });
        let mdns = mdns_sd::ServiceDaemon::new().unwrap();
        let found = mdns.browse("_matterc._udp.local.").unwrap();
        let wanted = format!("D={}", settings.discriminator);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = false;
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, found.recv_async()).await {
            if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
                let text: Vec<String> = info
                    .get_properties()
                    .iter()
                    .map(|p| p.to_string())
                    .collect();
                if text.contains(&wanted) {
                    seen = true;
                    break;
                }
            }
        }
        let _ = mdns.shutdown();
        let _ = stop.send(());
        running.join().unwrap().unwrap();
        let _ = std::fs::remove_dir_all(dir);
        assert!(seen, "not advertised with {wanted}");
    }
}
