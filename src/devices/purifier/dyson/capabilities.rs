//! The purifier as capabilities, for the menu bar and the CLI. HomeKit shows
//! the same state through `accessory`, and both send the same intents.

use crate::config::Settings as _;
use crate::connectors::dyson::protocol::{sweep, Intent, MachineState, SWEEPS};
use crate::{
    capabilities::{Capability, Item, Set, Snapshot},
    config::DysonDevice,
};
use anyhow::{anyhow, Result};

pub fn snapshot(device: &DysonDevice, state: &MachineState) -> Snapshot {
    let online = state.topic_prefix.is_some() || state.temperature_c.is_some();
    let headline = match (online, state.power_on) {
        (false, _) => "Looking".into(),
        (true, false) => "Off".into(),
        (true, true) if state.auto => "Auto".into(),
        (true, true) => state
            .speed
            .map_or("On".into(), |speed| format!("Speed {speed}")),
    };
    let sweep_option = sweep(state)
        .and_then(|width| {
            SWEEPS
                .into_iter()
                .min_by_key(|preset| preset.abs_diff(width))
        })
        .map(|width| width.to_string());
    let sensing = state.sensing();
    let reading = |id, name, unit, value: Option<f32>| {
        Capability::reading(id, name, unit, value).active(sensing && value.is_some())
    };

    let mut capabilities = vec![
        Capability::power(state.power_on),
        Capability::switch("auto", "Auto", state.auto),
        Capability::level(
            "speed",
            "Speed",
            (1.0, 10.0, 1.0),
            "",
            state.speed.map(f32::from),
        )
        .active(!state.auto),
        Capability::switch("oscillation", "Oscillation", state.oscillating),
        Capability::choice(
            "sweep",
            "Sweep",
            SWEEPS
                .iter()
                .map(|width| Item::new(width.to_string(), format!("{width}°")))
                .collect(),
            sweep_option,
        ),
        Capability::switch("night", "Night mode", state.night),
        Capability::switch("jet_focus", "Jet focus", state.focus),
        Capability::switch("monitoring", "Continuous monitoring", state.monitoring),
    ];
    if device.heating {
        capabilities.push(Capability::switch("heat", "Heat", state.heat_on));
        capabilities.push(Capability::level(
            "heat_target",
            "Heat to",
            (1.0, 37.0, 1.0),
            "°C",
            state.heat_target_c.map(f32::round),
        ));
    }
    capabilities.extend([
        reading("temperature", "Temperature", "°C", state.temperature_c),
        reading("humidity", "Humidity", "%", state.humidity),
        reading("pm25", "PM2.5", "µg/m³", state.pm25),
        reading("pm10", "PM10", "µg/m³", state.pm10),
        reading("formaldehyde", "Formaldehyde", "mg/m³", state.hcho_mg_m3),
        Capability::reading("filter", "Filter life", "%", state.hepa_life.map(f32::from)),
    ]);

    Snapshot {
        id: device.id(),
        kind: "dyson",
        category: "purifier",
        name: device.name.clone(),
        online,
        headline,
        detail: device.model.clone(),
        capabilities,
    }
}

/// What a request asks of the fan, in the intents HomeKit sends too.
pub fn intent(set: &Set) -> Result<Intent> {
    Ok(match set.capability.as_str() {
        "power" => Intent::Power(set.bool()?),
        "auto" => Intent::Auto(set.bool()?),
        "speed" => Intent::Speed(set.number()?.round().clamp(1.0, 10.0) as u8),
        "oscillation" => Intent::Swing(set.bool()?),
        "sweep" => {
            let width = set.option()?;
            let width = width
                .parse()
                .ok()
                .filter(|width| SWEEPS.contains(width))
                .ok_or_else(|| anyhow!("sweep takes 45, 90, 180 or 350, not {width}"))?;
            Intent::Sweep(width)
        }
        "night" => Intent::Night(set.bool()?),
        "jet_focus" => Intent::Focus(set.bool()?),
        "monitoring" => Intent::Monitor(set.bool()?),
        "heat" => Intent::Heat(set.bool()?),
        "heat_target" => Intent::HeatTargetC(set.number()?),
        _ => return Err(set.unknown()),
    })
}
