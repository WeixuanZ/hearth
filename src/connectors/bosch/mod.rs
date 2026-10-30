//! Bosch Home Connect, on the appliance's own websocket.
//!
//! A Home Connect sign-in, once, saves the machine's local key (see
//! `setup`). After that the bridge talks to the dishwasher on the LAN.

mod cloud;
pub mod link;
pub mod protocol;
mod xml;

mod setup;

pub use setup::Setup;
