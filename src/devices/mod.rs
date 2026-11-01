//! Each kind of device, one module per model: its task, its HomeKit
//! accessories, its capabilities.

pub mod cycle;
pub mod dishwasher;
pub mod purifier;
pub mod tv;

use crate::config::Device;
use anyhow::Result;
use hap::server::IpServer;

/// Starts a device's task, and adds its accessories to the bridge.
pub async fn spawn(device: Device, server: IpServer) -> Result<()> {
    match device {
        Device::Dyson(device) => purifier::dyson::spawn(device, server).await,
        Device::Frame(device) => tv::samsung_frame::spawn(device, server).await,
        Device::Bosch(device) => dishwasher::bosch::spawn(device, server).await,
    }
}
