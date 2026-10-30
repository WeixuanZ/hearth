use futures::future::{join_all, BoxFuture};
use log::debug;
use serde_json::Value;
use std::fmt::Debug;
use uuid::Uuid;

#[derive(Debug)]
pub enum Event {
    ControllerPaired { id: Uuid },
    ControllerUnpaired { id: Uuid },
    CharacteristicValueChanged { aid: u64, iid: u64, value: Value },
}

/// Hears every event. Answers whether it still wants them; one that says no,
/// like a closed connection's, is dropped.
pub type Listener = Box<dyn (Fn(&Event) -> BoxFuture<bool>) + Send + Sync>;

#[derive(Default)]
pub struct EventEmitter {
    listeners: Vec<Listener>,
}

impl EventEmitter {
    pub fn new() -> EventEmitter { EventEmitter::default() }

    pub fn add_listener(&mut self, listener: Listener) { self.listeners.push(listener); }

    pub async fn emit(&mut self, event: &Event) {
        debug!("emitting event: {:?}", event);

        let keep = join_all(self.listeners.iter().map(|listener| listener(event))).await;
        let mut keep = keep.into_iter();
        self.listeners.retain(|_| keep.next().unwrap_or(true));
    }
}
