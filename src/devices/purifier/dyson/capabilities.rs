//! The purifier as capabilities, for the menu bar and the CLI. HomeKit shows
//! the same state through `accessory`, and both send the same intents.

use crate::config::Settings as _;
use crate::connectors::dyson::protocol::{sweep, Intent, MachineState, SWEEPS};
use crate::{
    capabilities::{About, Alert, Capability, Item, Measure, Role, Set, Snapshot},
    devices::purifier::dyson::DysonDevice,
};
use anyhow::{anyhow, Result};

/// A filter this worn is due for replacing.
const FILTER_LOW: u8 = 10;
/// PM2.5 in µg/m³ above which air counts as unhealthy for sensitive groups.
const PM25_HIGH: f32 = 35.0;

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
    let measured = |measure, value: Option<f32>| {
        Capability::measure(measure, value).active(sensing && value.is_some())
    };

    let mut capabilities = vec![
        Capability::power(state.power_on),
        Capability::switch("auto", "Auto", state.auto).role(Role::Auto),
        Capability::level(
            "speed",
            "Speed",
            (1.0, 10.0, 1.0),
            "",
            state.speed.map(f32::from),
        )
        .role(Role::FanSpeed)
        .active(!state.auto),
        Capability::switch("oscillation", "Oscillation", state.oscillating).role(Role::Oscillation),
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
        capabilities.push(Capability::switch("heat", "Heat", state.heat_on).role(Role::Heat));
        capabilities.push(
            Capability::level(
                "heat_target",
                "Heat to",
                (1.0, 37.0, 1.0),
                "°C",
                state.heat_target_c.map(f32::round),
            )
            .role(Role::HeatTarget),
        );
    }
    capabilities.extend([
        measured(Measure::Temperature, state.temperature_c),
        measured(Measure::Humidity, state.humidity),
        measured(Measure::Pm25, state.pm25),
        measured(Measure::Pm10, state.pm10),
        measured(Measure::Formaldehyde, state.hcho_mg_m3),
        measured(Measure::Voc, state.voc_index),
        measured(Measure::No2, state.no2_index),
        // The filter is known whether or not the sensors are measuring.
        Capability::measure(Measure::FilterLife, state.hepa_life.map(f32::from)),
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
        alerts: if online { alerts(state) } else { Vec::new() },
        about: About {
            maker: "Dyson".into(),
            model: device.model.clone(),
            serial: device.serial.clone(),
            firmware: "1.0".into(),
        },
    }
}

fn alerts(state: &MachineState) -> Vec<Alert> {
    let mut alerts = Vec::new();
    if let Some(life) = state.hepa_life.filter(|life| *life <= FILTER_LOW) {
        alerts.push(Alert::new(
            "filter",
            format!("Filter life is down to {life}%. Time to replace it."),
        ));
    }
    let pm25 = state.pm25.filter(|_| state.sensing());
    if let Some(pm25) = pm25.filter(|pm25| *pm25 > PM25_HIGH) {
        alerts.push(Alert::new(
            "pm25",
            format!("PM2.5 is {pm25:.0} µg/m³. The air is unhealthy."),
        ));
    }
    alerts
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

#[cfg(test)]
mod tests {
    use super::*;

    fn alerts(state: MachineState) -> Vec<&'static str> {
        let device = DysonDevice {
            aid: 2,
            name: "Purifier".into(),
            serial: "ABC-XY-12345678".into(),
            model: "HP09".into(),
            product_type: "auto".into(),
            mqtt_password: "secret".into(),
            host: String::new(),
            port: 1883,
            heating: true,
        };
        let snap = snapshot(&device, &state);
        snap.alerts.iter().map(|alert| alert.id).collect()
    }

    #[test]
    fn a_worn_filter_and_dirty_air_are_alerts() {
        let measuring = MachineState {
            temperature_c: Some(20.0),
            monitoring: true,
            ..MachineState::default()
        };
        let clean = MachineState {
            hepa_life: Some(11),
            pm25: Some(35.0),
            ..measuring.clone()
        };
        assert_eq!(alerts(clean), Vec::<&str>::new());
        let worn = MachineState {
            hepa_life: Some(10),
            pm25: Some(36.0),
            ..measuring
        };
        assert_eq!(alerts(worn), ["filter", "pm25"]);
    }

    #[test]
    fn a_stale_reading_raises_nothing() {
        let resting = MachineState {
            temperature_c: Some(20.0),
            pm25: Some(80.0),
            ..MachineState::default()
        };
        assert_eq!(alerts(resting), Vec::<&str>::new());
        assert_eq!(alerts(MachineState::default()), Vec::<&str>::new());
    }
}
