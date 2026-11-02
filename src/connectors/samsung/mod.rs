//! What every Samsung TV shares, on the LAN only, never SmartThings: secure
//! websocket channels on port 8002 (the remote, and apps' channels like the
//! Frame's art), pairing, IP Control on port 1516 for power and inputs,
//! finding a TV, and a wake packet when one is fully off. Each model's own
//! messages live with the model.
//!
//! On the TV, turn on Settings → General → External Device Manager → Device
//! Connection Manager → Access Notification, Connection → Power On with
//! Mobile, and Network → Expert Settings → IP Remote.

pub mod discover;
pub mod ipc;
pub mod link;
pub mod protocol;
