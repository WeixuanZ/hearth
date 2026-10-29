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

use crate::config::{self, FrameDevice};
use anyhow::{anyhow, Result};
use ipc::IpControl;
use std::time::Duration;

pub async fn pair(host: Option<String>, name: String) -> Result<()> {
    let host = match host {
        Some(host) => host,
        None => find_one().await?,
    };
    println!("Turn {host} on. If the screen asks, press Allow.");
    let token = link::pair(&host).await?;
    let info = link::device_info(&host).await.ok();
    if let Some(info) = &info {
        if !info.model.is_empty() {
            println!("The TV says it is {}.", info.model);
        }
        if !info.frame {
            println!("It did not describe itself as a Frame. Art mode may do nothing.");
        }
    }
    println!("Now IP Control, for power and inputs. Leave art mode, then press Allow again.");
    let ip_token = match IpControl::pair(&host).await {
        Ok(token) => token,
        Err(error) => {
            println!("Skipped IP Control ({error:#}). Inputs fall back to remote keys.");
            println!("Turn on Settings → General → Network → Expert Settings → IP Remote,");
            println!("then run this again.");
            String::new()
        }
    };
    let mac = info
        .as_ref()
        .map(|info| info.mac.clone())
        .unwrap_or_default();
    let mut saved = config::load_or_new()?;
    if saved.bridge_id.is_empty() {
        saved.bridge_id = if mac.is_empty() {
            host.clone()
        } else {
            mac.clone()
        };
    }
    config::upsert_frame(
        &mut saved,
        FrameDevice {
            name,
            model: info.map(|info| info.model).unwrap_or_default(),
            host,
            mac,
            token,
            ip_token,
            ..Default::default()
        },
    );
    let path = config::save(&saved)?;
    println!("Saved {}.", path.display());
    println!("Apple Home pairing code: {}", saved.homekit_pin);
    println!("Next: hearth run");
    Ok(())
}

async fn find_one() -> Result<String> {
    println!("Looking for a Samsung TV on the network.");
    let hosts = discover::search(Duration::from_secs(4)).await?;
    match hosts.as_slice() {
        [host] => Ok(host.clone()),
        [] => Err(anyhow!(
            "no Samsung TV answered. Turn it on and pass --host"
        )),
        many => {
            println!("More than one TV answered:");
            for host in many {
                println!("  {host}");
            }
            Err(anyhow!("pass --host with the one you want"))
        }
    }
}
