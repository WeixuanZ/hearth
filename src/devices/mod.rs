//! Each kind of device, one module per model: its task and its
//! capabilities, on the runtime every device shares.

pub mod cycle;
pub mod dishwasher;
pub mod generic;
pub mod purifier;
pub mod runtime;
pub mod tv;

use crate::{config::Device, driver};
use anyhow::Result;

/// Starts a device's task, through the driver for its kind. A kind this
/// build does not know is left for a computer that does.
pub async fn spawn(device: Device) -> Result<()> {
    let Some(driver) = driver::find(device.kind()) else {
        log::warn!(
            "{} is a {} device, which this version of hearth does not know",
            device.name(),
            device.kind()
        );
        return Ok(());
    };
    driver
        .start(&device, driver::Context::new(device.id()))
        .await
}
