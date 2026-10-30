//! Bosch Home Connect, on the appliance's own websocket.
//!
//! `hearth bosch login` signs in once and saves the machine's local key.
//! After that the bridge talks to the dishwasher on the LAN.

mod cloud;
pub mod link;
pub mod protocol;
mod xml;

pub use cloud::{login, submit};
