//! `hearth doctor`: what stands between Home and the devices, checked in turn.
//!
//! It only looks. The bridge is asked for its status like the CLI asks, and a
//! device it cannot reach is dialled and hung up on, with nothing sent.

use super::{cli::Style, control};
use crate::config;
use anyhow::Result;
use futures_util::future::join_all;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::net::TcpStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Finding {
    check: String,
    verdict: Verdict,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
}

impl Finding {
    fn new(check: &str, verdict: Verdict, detail: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            verdict,
            detail: detail.into(),
            fix: None,
        }
    }

    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

pub async fn run(as_json: bool) -> Result<()> {
    let findings = examine().await;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&findings)?);
    } else {
        print(&findings);
    }
    let failed = findings
        .iter()
        .filter(|finding| finding.verdict == Verdict::Fail)
        .count();
    if failed > 0 {
        anyhow::bail!("{failed} of {} checks failed", findings.len());
    }
    Ok(())
}

async fn examine() -> Vec<Finding> {
    let config = match config::load() {
        Ok(config) => config,
        Err(error) => {
            return vec![Finding::new("Config", Verdict::Fail, format!("{error:#}"))
                .fix("Start the bridge once, with `hearth run` or the menu bar, to create it")]
        }
    };
    let status = control::request(json!({ "op": "status" })).await;
    let mut findings = vec![
        Finding::new(
            "Config",
            Verdict::Ok,
            match config.devices.len() {
                1 => "1 device".into(),
                count => format!("{count} devices"),
            },
        ),
        bridge(&status),
        network(),
    ];
    #[cfg(target_os = "macos")]
    findings.extend(firewall(status.as_ref().ok()));
    let seen = status
        .as_ref()
        .ok()
        .and_then(|status| status["devices"].as_array().cloned())
        .unwrap_or_default();
    let devices = config.devices.iter().map(|device| {
        let settings = device.settings();
        let (id, name) = (settings.id(), settings.name().to_owned());
        let (address, incomplete) = (settings.address(), settings.incomplete());
        let seen = seen
            .iter()
            .find(|device| device["id"] == id.as_str())
            .cloned();
        // Dialled only when the bridge cannot reach it: some take one client
        // at a time, and the bridge's session is the one that matters.
        let online = seen
            .as_ref()
            .is_some_and(|seen| seen["online"].as_bool() == Some(true));
        async move {
            let reached = match &address {
                Some((host, port)) if !online => Some(dial(host, *port).await),
                _ => None,
            };
            device_finding(&name, incomplete, address.as_ref(), reached, seen.as_ref())
        }
    });
    findings.extend(join_all(devices).await);
    findings
}

fn bridge(status: &Result<Value>) -> Finding {
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            return Finding::new("Bridge", Verdict::Warn, format!("{error:#}"))
                .fix("Start it with `hearth run`, or from the menu bar")
        }
    };
    let members = status["group"]["members"].as_array().map_or(0, Vec::len);
    let group = match members {
        0 | 1 => String::new(),
        count => format!(", one of {count} computers in the group"),
    };
    if status["paired"].as_bool() == Some(true) {
        Finding::new("Bridge", Verdict::Ok, format!("running, in Home{group}"))
    } else {
        Finding::new(
            "Bridge",
            Verdict::Warn,
            format!("running, not yet in Home{group}"),
        )
        .fix("In Home, Add Accessory, and type the code `hearth pin` prints")
    }
}

/// The address this computer would use on the local network, found by asking
/// for a route to the mDNS group Home is announced on. Nothing is sent.
fn network() -> Finding {
    let local = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|socket| {
        socket.connect("224.0.0.251:5353")?;
        socket.local_addr()
    });
    match local {
        Ok(local) if !local.ip().is_loopback() && !local.ip().is_unspecified() => {
            Finding::new("Network", Verdict::Ok, format!("on {}", local.ip()))
        }
        _ => Finding::new("Network", Verdict::Fail, "no local network")
            .fix("Join the same network as the devices and your Home hub"),
    }
}

#[cfg(target_os = "macos")]
fn firewall(status: Option<&Value>) -> Option<Finding> {
    let ask = |args: &[&str]| {
        std::process::Command::new("/usr/libexec/ApplicationFirewall/socketfilterfw")
            .args(args)
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    // The bridge that is running, else this binary, which is the one `hearth run` starts.
    let bridge = status
        .and_then(|status| status["pid"].as_u64())
        .and_then(|pid| {
            let output = std::process::Command::new("ps")
                .args(["-o", "comm=", "-p", &pid.to_string()])
                .output()
                .ok()?;
            let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            (!path.is_empty()).then_some(path)
        })
        .or_else(|| Some(std::env::current_exe().ok()?.display().to_string()))?;
    Some(firewall_finding(
        &ask(&["--getglobalstate"])?,
        &ask(&["--getblockall"])?,
        &ask(&["--getappblocked", &bridge])?,
    ))
}

/// From what `socketfilterfw` printed: whether it is on, whether it blocks
/// everything, and what it does with the bridge.
#[cfg(any(target_os = "macos", test))]
fn firewall_finding(state: &str, block_all: &str, bridge: &str) -> Finding {
    if !state.contains("enabled") {
        return Finding::new("Firewall", Verdict::Ok, "off");
    }
    if block_all.contains("enabled") {
        return Finding::new(
            "Firewall",
            Verdict::Fail,
            "blocks every incoming connection, so Home cannot reach the bridge",
        )
        .fix("System Settings > Network > Firewall > Options: turn off Block all incoming connections");
    }
    if bridge.contains("is permitted") {
        Finding::new("Firewall", Verdict::Ok, "on, and lets the bridge in")
    } else if bridge.contains("is blocked") {
        Finding::new("Firewall", Verdict::Fail, "on, and blocks the bridge").fix(
            "System Settings > Network > Firewall > Options: allow incoming connections for Hearth",
        )
    } else {
        Finding::new(
            "Firewall",
            Verdict::Warn,
            "on, and has not been told about the bridge",
        )
        .fix("Allow incoming connections when macOS asks, or add Hearth in Firewall > Options")
    }
}

async fn dial(host: &str, port: u16) -> bool {
    let connect = TcpStream::connect((host, port));
    matches!(
        tokio::time::timeout(Duration::from_secs(3), connect).await,
        Ok(Ok(_))
    )
}

/// A device, from its settings, whether its address answered, and how the
/// bridge last saw it.
fn device_finding(
    name: &str,
    incomplete: Option<&str>,
    address: Option<&(String, u16)>,
    reached: Option<bool>,
    seen: Option<&Value>,
) -> Finding {
    if let Some(todo) = incomplete {
        return Finding::new(name, Verdict::Warn, todo);
    }
    if let (Some((host, port)), Some(false)) = (address, reached) {
        return Finding::new(name, Verdict::Fail, format!("{host}:{port} did not answer"))
            .fix("Check it is switched on and on the same network as this computer");
    }
    let headline = seen
        .and_then(|seen| seen["headline"].as_str())
        .unwrap_or_default();
    match seen.map(|seen| seen["online"].as_bool() == Some(true)) {
        Some(true) => Finding::new(name, Verdict::Ok, headline),
        Some(false) => Finding::new(
            name,
            Verdict::Warn,
            format!("the bridge has not reached it ({})", headline.to_lowercase()),
        )
        .fix("It may still be connecting. If it stays like this, `RUST_LOG=hearth=debug hearth run` says why"),
        None if reached == Some(true) => Finding::new(name, Verdict::Ok, "answers on the network"),
        None => Finding::new(name, Verdict::Ok, "found on the network when the bridge runs"),
    }
}

fn print(findings: &[Finding]) {
    let style = Style::detect();
    let width = findings
        .iter()
        .map(|finding| finding.check.chars().count())
        .max()
        .unwrap_or(0);
    for finding in findings {
        let mark = match finding.verdict {
            Verdict::Ok => style.wrap("32", "✓"),
            Verdict::Warn => style.wrap("33", "!"),
            Verdict::Fail => style.wrap("31", "✗"),
        };
        let check = format!("{:width$}", finding.check);
        println!("{mark} {}  {}", style.bold(&check), finding.detail);
        if let Some(fix) = &finding.fix {
            println!("  {:width$}  {}", "", style.dim(&format!("→ {fix}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bridge_is_fine_once_home_has_it() {
        let status = Ok(json!({ "paired": true, "group": { "members": [{}, {}] } }));
        assert_eq!(
            bridge(&status),
            Finding::new(
                "Bridge",
                Verdict::Ok,
                "running, in Home, one of 2 computers in the group"
            )
        );
        let status = Ok(json!({ "paired": false, "group": { "members": [{}] } }));
        assert_eq!(bridge(&status).verdict, Verdict::Warn);
        assert_eq!(
            bridge(&Err(anyhow::anyhow!("not running"))).verdict,
            Verdict::Warn
        );
    }

    #[test]
    fn the_firewall_fails_only_when_it_shuts_the_bridge_out() {
        let on = "Firewall is enabled. (State = 1)";
        let open = "Firewall has block all state set to disabled.";
        let shut = "Firewall has block all state set to enabled.";
        let allowed = "Incoming connection to /opt/hearth is permitted.";
        let blocked = "Incoming connection to /opt/hearth is blocked.";
        let verdict = |state, all, app| firewall_finding(state, all, app).verdict;
        assert_eq!(
            verdict("Firewall is disabled. (State = 0)", shut, blocked),
            Verdict::Ok
        );
        assert_eq!(verdict(on, open, allowed), Verdict::Ok);
        assert_eq!(verdict(on, open, blocked), Verdict::Fail);
        assert_eq!(verdict(on, shut, allowed), Verdict::Fail);
        assert_eq!(verdict(on, open, ""), Verdict::Warn);
    }

    #[test]
    fn a_device_is_judged_by_its_address_then_by_the_bridge() {
        let address = ("10.0.0.2".to_owned(), 8001);
        let online = json!({ "online": true, "headline": "Art" });
        let offline = json!({ "online": false, "headline": "Unreachable" });
        let judge = |incomplete, reached, seen: Option<&Value>| {
            device_finding("TV", incomplete, Some(&address), reached, seen)
        };

        assert_eq!(
            judge(None, Some(true), Some(&online)),
            Finding::new("TV", Verdict::Ok, "Art")
        );
        let silent = judge(None, Some(false), Some(&online));
        assert_eq!(
            (silent.verdict, silent.detail.as_str()),
            (Verdict::Fail, "10.0.0.2:8001 did not answer")
        );
        assert_eq!(
            judge(None, Some(true), Some(&offline)).verdict,
            Verdict::Warn
        );
        assert_eq!(
            judge(Some("not paired"), Some(true), Some(&online)).verdict,
            Verdict::Warn
        );
        assert_eq!(judge(None, Some(true), None).verdict, Verdict::Ok);
        let found = device_finding("Fan", None, None, None, None);
        assert_eq!(found.verdict, Verdict::Ok);
    }
}
