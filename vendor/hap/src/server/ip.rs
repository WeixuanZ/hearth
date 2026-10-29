use async_trait::async_trait;
use futures::{
    future::{BoxFuture, FutureExt},
    lock::Mutex,
};
use log::{error, info};
use std::sync::Arc;

use crate::{
    accessory::HapAccessory,
    config::Config,
    event::{Event, EventEmitter},
    pointer,
    server::Server,
    storage::{accessory_database::AccessoryDatabase, Storage},
    transport::{http::server::Server as HttpServer, mdns::MdnsResponder},
    BonjourStatusFlag,
    Result,
};

/// HAP Server via TCP/IP.
#[derive(Clone)]
pub struct IpServer {
    config: pointer::Config,
    storage: pointer::Storage,
    accessory_database: pointer::AccessoryDatabase,
    http_server: HttpServer,
    mdns_responder: pointer::MdnsResponder,
    aid_cache: Arc<Mutex<Vec<u64>>>,
}

impl IpServer {
    /// Creates a new [`IpServer`](IpServer).
    ///
    /// # Examples
    /// ```no_run
    /// use tokio;
    ///
    /// use hap::{
    ///     accessory::{lightbulb::LightbulbAccessory, AccessoryCategory, AccessoryInformation},
    ///     server::{IpServer, Server},
    ///     storage::{FileStorage, Storage},
    ///     Config,
    ///     MacAddress,
    ///     Pin,
    ///     Result,
    /// };
    ///
    /// #[tokio::main]
    /// async fn main() -> Result<()> {
    ///     let lightbulb = LightbulbAccessory::new(1, AccessoryInformation {
    ///         name: "Acme Lightbulb".into(),
    ///         ..Default::default()
    ///     })?;
    ///
    ///     let mut storage = FileStorage::current_dir().await?;
    ///
    ///     let config = match storage.load_config().await {
    ///         Ok(mut config) => {
    ///             config.redetermine_local_ip();
    ///             storage.save_config(&config).await?;
    ///             config
    ///         },
    ///         Err(_) => {
    ///             let config = Config {
    ///                 pin: Pin::new([1, 1, 1, 2, 2, 3, 3, 3])?,
    ///                 name: "Acme Lightbulb".into(),
    ///                 device_id: MacAddress::new([10, 20, 30, 40, 50, 60]),
    ///                 category: AccessoryCategory::Lightbulb,
    ///                 ..Default::default()
    ///             };
    ///             storage.save_config(&config).await?;
    ///             config
    ///         },
    ///     };
    ///
    ///     let mut server = IpServer::new(config, storage).await?;
    ///     server.add_accessory(lightbulb).await?;
    ///
    ///     let handle = server.run_handle();
    ///
    ///     std::env::set_var("RUST_LOG", "hap=info");
    ///     env_logger::init();
    ///
    ///     handle.await
    /// }
    /// ```
    pub async fn new<S: Storage + Send + Sync + 'static>(config: Config, storage: S) -> Result<Self> {
        let config = Arc::new(Mutex::new(config));
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(storage)));

        let config_ = config.clone();
        let storage_ = storage.clone();
        let mut event_emitter = EventEmitter::new();

        let mut s = storage_.lock().await;
        if s.count_pairings().await? > 0 {
            info!("1 or more controllers paired; setting Bonjour status flag to `Zero`");

            let mut c = config_.lock().await;
            c.status_flag = BonjourStatusFlag::Zero;

            s.save_config(&c).await?;
        } else {
            info!("0 controllers paired; setting Bonjour status flag to `Not Paired`");

            let mut c = config_.lock().await;
            c.status_flag = BonjourStatusFlag::NotPaired;

            s.save_config(&c).await?;
        }
        drop(s);

        let mdns_responder = Arc::new(Mutex::new(MdnsResponder::new(config.clone()).await));
        let mdns_responder_ = mdns_responder.clone();

        event_emitter.add_listener(Box::new(move |event| {
            let config_ = config_.clone();
            let storage_ = storage_.clone();
            let mdns_responder_ = mdns_responder_.clone();
            async move {
                match *event {
                    Event::ControllerPaired { id } => {
                        info!("controller {} paired", id);

                        let pairing_count = storage_.lock().await.count_pairings().await;
                        if let Ok(count) = pairing_count {
                            if count > 0 {
                                info!("1 or more controllers paired; setting Bonjour status flag to `Zero`");

                                let mut c = config_.lock().await;
                                c.status_flag = BonjourStatusFlag::Zero;

                                storage_
                                    .lock()
                                    .await
                                    .save_config(&c)
                                    .await
                                    .map_err(|e| error!("error saving the config: {:?}", e))
                                    .ok();

                                drop(c);

                                mdns_responder_.lock().await.update_records().await;
                            }
                        }
                    },
                    Event::ControllerUnpaired { id } => {
                        info!("controller {} unpaired", id);

                        let pairing_count = storage_.lock().await.count_pairings().await;
                        // TODO - `pairing_count` is an Err
                        if let Ok(count) = pairing_count {
                            if count == 0 {
                                info!("0 controllers paired; setting Bonjour status flag to `Not Paired`");

                                let mut c = config_.lock().await;
                                c.status_flag = BonjourStatusFlag::NotPaired;

                                storage_
                                    .lock()
                                    .await
                                    .save_config(&c)
                                    .await
                                    .map_err(|e| error!("error saving the config: {:?}", e))
                                    .ok();

                                drop(c);

                                mdns_responder_.lock().await.update_records().await;
                            }
                        }
                    },
                    _ => {},
                }
            }
            .boxed()
        }));

        let event_emitter = Arc::new(Mutex::new(event_emitter));
        let accessory_database = Arc::new(Mutex::new(AccessoryDatabase::new(event_emitter.clone())));

        let http_server = HttpServer::new(
            config.clone(),
            storage.clone(),
            accessory_database.clone(),
            event_emitter,
            mdns_responder.clone(),
        );

        let mut storage_lock = storage.lock().await;
        let aid_cache = Arc::new(Mutex::new(match storage_lock.load_aid_cache().await {
            Ok(aid_cache) => aid_cache,
            Err(_) => {
                storage_lock.delete_aid_cache().await.ok();
                let aid_cache = Vec::new();
                storage_lock.save_aid_cache(&aid_cache).await?;
                aid_cache
            },
        }));
        drop(storage_lock);

        let server = IpServer {
            config,
            storage,
            accessory_database,
            http_server,
            mdns_responder,
            aid_cache,
        };

        Ok(server)
    }

    /// Bumps the configuration number when the accessory layout differs from
    /// the last run's, so Home fetches it again. Adding an accessory id is not
    /// the only change Home caches: services, characteristics and names count.
    async fn note_layout(&self) -> Result<()> {
        let json = self.accessory_database.lock().await.as_serialized_json().await?;
        let mut layout: serde_json::Value = serde_json::from_slice(&json)?;
        strip_readings(&mut layout);
        let layout = serde_json::to_vec(&layout)?;

        let mut storage = self.storage.lock().await;
        if storage.load_bytes("accessory_layout").await.ok().as_deref() == Some(&layout[..]) {
            return Ok(());
        }
        let mut config = self.config.lock().await;
        config.configuration_number = config.configuration_number % 65535 + 1;
        info!("accessory layout changed; configuration number is now {}", config.configuration_number);
        storage.save_config(&config).await?;
        storage.save_bytes("accessory_layout", &layout).await?;
        Ok(())
    }
}

/// Drops the readings from a layout, so a temperature change is not a new
/// layout. Text stays: a name is part of what Home shows.
fn strip_readings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("format").and_then(|format| format.as_str()) != Some("string") {
                map.remove("value");
            }
            map.values_mut().for_each(strip_readings);
        },
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_readings),
        _ => {},
    }
}

#[async_trait]
impl Server for IpServer {
    fn run_handle(&self) -> BoxFuture<'_,Result<()>> {
        let http_handle = self.http_server.run_handle();
        let mdns_responder = self.mdns_responder.clone();
        let this = self.clone();

        let handle = async move {
            this.note_layout().await?;
            let mdns_handle = mdns_responder.lock().await.run_handle();

            futures::try_join!(http_handle, mdns_handle.map(|_| Ok(())))?;

            Ok(())
        }
        .boxed();

        Box::pin(handle)
    }

    fn config_pointer(&self) -> pointer::Config { self.config.clone() }

    fn storage_pointer(&self) -> pointer::Storage { self.storage.clone() }

    async fn add_accessory<A: HapAccessory + 'static>(&self, accessory: A) -> Result<pointer::Accessory> {
        let aid = accessory.get_id();

        let accessory = self
            .accessory_database
            .lock()
            .await
            .add_accessory(Box::new(accessory))?;

        let mut aid_cache = self.aid_cache.lock().await;
        if !aid_cache.contains(&aid) {
            aid_cache.push(aid);
            self.storage.lock().await.save_aid_cache(&aid_cache).await?;

            let mut config = self.config.lock().await;
            config.configuration_number += 1;
            self.storage.lock().await.save_config(&config).await?;
        }

        Ok(accessory)
    }

    async fn remove_accessory(&self, accessory: &pointer::Accessory) -> Result<()> {
        let aid = accessory.lock().await.get_id();

        self.accessory_database
            .lock()
            .await
            .remove_accessory(&accessory)
            .await?;

        let mut aid_cache = self.aid_cache.lock().await;
        if aid_cache.contains(&aid) {
            aid_cache.retain(|id| *id != aid);
            self.storage.lock().await.save_aid_cache(&aid_cache).await?;

            let mut config = self.config.lock().await;
            config.configuration_number += 1;
        }

        Ok(())
    }

    // async fn factory_reset(&mut self) -> Result<()> {
    //     unimplemented!();

    //     Ok(())
    // }
}
