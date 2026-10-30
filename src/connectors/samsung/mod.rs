//! Samsung TVs, on the LAN only, never SmartThings: a secure websocket on port
//! 8002 for remote keys and art mode, IP Control on port 1516 for power and
//! inputs, and a wake packet when a TV is fully off.
//!
//! On the TV, turn on Settings → General → External Device Manager → Device
//! Connection Manager → Access Notification, Connection → Power On with
//! Mobile, and Network → Expert Settings → IP Remote.

mod discover;
pub mod ipc;
pub mod link;
pub mod protocol;
mod setup;

pub use setup::Setup;
