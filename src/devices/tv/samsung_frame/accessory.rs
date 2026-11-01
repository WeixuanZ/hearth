//! The Frame, as a HomeKit television and an art mode switch.
//!
//! Power is the television's own on and off. Art mode is a separate accessory:
//! Home shows a TV as power and an input picker only, and lists any service
//! linked to it as an input. Brightness is the art-mode brightness. Volume
//! buttons send the TV's own volume keys. Navigation is Control Center's remote.

use crate::config::Settings as _;
use crate::connectors::samsung::protocol::Input;
use crate::{
    devices::tv::samsung_frame::FrameDevice,
    surfaces::homekit::characteristics::{
        configured_name, device_name, hook_bool, hook_i32, hook_u32, hook_u8, put,
    },
};
use anyhow::{anyhow, Result};
use hap::{
    accessory::{switch::SwitchAccessory, AccessoryInformation, HapAccessory},
    characteristic::HapCharacteristic,
    service::{
        accessory_information::AccessoryInformationService, input_source::InputSourceService,
        speaker::SpeakerService, switch::SwitchService, television::TelevisionService, HapService,
    },
    HapType,
};
use serde::ser::{Serialize, SerializeStruct, Serializer};
use serde_json::json;
use std::sync::{atomic::AtomicBool, Arc};
use tokio::sync::mpsc;

use super::Command;

pub struct FrameAccessory {
    id: u64,
    info: AccessoryInformationService,
    television: TelevisionService,
    speaker: SpeakerService,
    inputs: Vec<InputSourceService>,
}

impl HapAccessory for FrameAccessory {
    fn get_id(&self) -> u64 {
        self.id
    }
    fn set_id(&mut self, id: u64) {
        self.id = id;
    }
    fn get_service(&self, hap_type: HapType) -> Option<&dyn HapService> {
        self.get_services()
            .into_iter()
            .find(|service| service.get_type() == hap_type)
    }
    fn get_mut_service(&mut self, hap_type: HapType) -> Option<&mut dyn HapService> {
        self.get_mut_services()
            .into_iter()
            .find(|service| service.get_type() == hap_type)
    }
    fn get_services(&self) -> Vec<&dyn HapService> {
        let mut services: Vec<&dyn HapService> = vec![&self.info, &self.television, &self.speaker];
        services.extend(self.inputs.iter().map(|input| input as &dyn HapService));
        services
    }
    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        let mut services: Vec<&mut dyn HapService> =
            vec![&mut self.info, &mut self.television, &mut self.speaker];
        services.extend(
            self.inputs
                .iter_mut()
                .map(|input| input as &mut dyn HapService),
        );
        services
    }
}

impl Serialize for FrameAccessory {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("HapAccessory", 2)?;
        state.serialize_field("aid", &self.get_id())?;
        state.serialize_field("services", &self.get_services())?;
        state.end()
    }
}

fn info(device: &FrameDevice, name: &str, suffix: &str) -> AccessoryInformation {
    let serial = if device.mac.is_empty() {
        &device.host
    } else {
        &device.mac
    };
    AccessoryInformation {
        name: name.into(),
        manufacturer: "Samsung".into(),
        model: if device.model.is_empty() {
            "The Frame 2024".into()
        } else {
            device.model.clone()
        },
        serial_number: format!("{serial}{suffix}"),
        firmware_revision: Some("2024".into()),
        ..Default::default()
    }
}

pub async fn build(
    device: &FrameDevice,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Command>,
) -> Result<FrameAccessory> {
    let info = info(device, &device.name, "")
        .to_service(1, device.aid)
        .map_err(|error| anyhow!("{error}"))?;

    // Fixed iid slots. hap gives optional characteristics fixed offsets even
    // when absent (the information service reaches 14, the television 33), so
    // counting the characteristics that are present lands on taken iids.
    let mut television = TelevisionService::new(20, device.aid);
    television.set_primary(true);
    television.display_order = None;
    television.closed_captions = None;
    television.current_media_state = None;
    television.target_media_state = None;
    television.picture_mode = None;
    television.power_mode_selection = None;
    hook_u8(&mut television.active, suppress, tx, |value| {
        Command::Active(value == 1)
    });
    hook_u32(
        &mut television.active_identifier,
        suppress,
        tx,
        Command::Input,
    );
    hook_u8(&mut television.remote_key, suppress, tx, Command::Key);
    if let Some(brightness) = television.brightness.as_mut() {
        hook_i32(brightness, suppress, tx, Command::Brightness);
    }
    device_name(&mut television.configured_name, device.id(), &device.name).await?;
    if let Some(name) = television.name.as_mut() {
        set(name, json!(device.name)).await?;
    }

    let mut speaker = SpeakerService::new(40, device.aid);
    speaker.volume = None;
    speaker.active = None;
    hook_bool(&mut speaker.mute, suppress, tx, |_| Command::Mute);
    if let Some(kind) = speaker.volume_control_type.as_mut() {
        set(kind, json!(1u8)).await?;
    }
    if let Some(selector) = speaker.volume_selector.as_mut() {
        hook_u8(selector, suppress, tx, Command::Volume);
    }

    let mut inputs = Vec::new();
    for (iid, input) in (50..).step_by(10).zip(Input::ALL) {
        let mut source = InputSourceService::new(iid, device.aid);
        configured_name(&mut source.configured_name, device.aid, input.label()).await?;
        set(&mut source.name, json!(input.label())).await?;
        set(&mut source.input_source_type, json!(input.source_type())).await?;
        set(&mut source.is_configured, json!(1u8)).await?;
        set(&mut source.current_visibility_state, json!(0u8)).await?;
        if let Some(identifier) = source.identifier.as_mut() {
            set(identifier, json!(input as u32)).await?;
        }
        if let Some(kind) = source.input_device_type.as_mut() {
            set(kind, json!(1u8)).await?;
        }
        if let Some(target) = source.target_visibility_state.as_mut() {
            set(target, json!(0u8)).await?;
        }
        inputs.push(source);
    }
    television.set_linked_services(inputs.iter().map(|source| source.get_id()).collect());

    Ok(FrameAccessory {
        id: device.aid,
        info,
        television,
        speaker,
        inputs,
    })
}

/// Art mode, as a tile of its own beside the TV.
pub async fn build_art(
    device: &FrameDevice,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Command>,
) -> Result<SwitchAccessory> {
    let aid = device.art_aid;
    let mut art = SwitchAccessory::new(aid, info(device, "Art mode", "-art"))
        .map_err(|error| anyhow!("{error}"))?;
    // hap puts the switch at iid 8, under the firmware revision's fixed iid 10.
    art.switch = SwitchService::new(20, aid);
    if let Some(name) = art.switch.name.as_mut() {
        set(name, json!("Art mode")).await?;
    }
    if let Some(name) = art.switch.configured_name.as_mut() {
        configured_name(name, aid, "Art mode").await?;
    }
    hook_bool(&mut art.switch.power_state, suppress, tx, Command::Art);
    Ok(art)
}

async fn set(characteristic: &mut impl HapCharacteristic, value: serde_json::Value) -> Result<()> {
    characteristic
        .set_value(value)
        .await
        .map_err(|error| anyhow!("{error}"))
}

#[derive(Clone)]
pub struct View {
    pub on: bool,
    pub art: bool,
    pub input: u32,
    pub brightness: Option<i32>,
}

pub async fn apply(accessory: &mut dyn HapAccessory, view: &View) -> Result<()> {
    put(
        accessory,
        HapType::Television,
        HapType::Active,
        json!(if view.on { 1 } else { 0 }),
    )
    .await?;
    if view.input != 0 {
        put(
            accessory,
            HapType::Television,
            HapType::ActiveIdentifier,
            json!(view.input),
        )
        .await?;
    }
    if let Some(brightness) = view.brightness {
        put(
            accessory,
            HapType::Television,
            HapType::Brightness,
            json!(brightness),
        )
        .await?;
    }
    Ok(())
}

pub async fn apply_art(accessory: &mut dyn HapAccessory, view: &View) -> Result<()> {
    put(
        accessory,
        HapType::Switch,
        HapType::PowerState,
        json!(view.art),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surfaces::homekit::characteristics::assert_unique_iids;

    #[tokio::test]
    async fn the_tv_links_only_inputs_and_art_stands_alone() {
        let device = FrameDevice {
            aid: 2,
            art_aid: 4,
            name: "Television".into(),
            model: "QE00LS03D".into(),
            host: "10.0.0.2".into(),
            mac: "00:11:22:33:44:55".into(),
            ..Default::default()
        };
        let suppress = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel(4);
        let frame = build(&device, &suppress, &tx).await.unwrap();
        assert_unique_iids(&frame);
        let value = serde_json::to_value(&frame).unwrap();
        let television = value["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|service| service["iid"] == 20)
            .unwrap();
        assert_eq!(television["linked"], json!([50, 60, 70, 80, 90]));

        let art = build_art(&device, &suppress, &tx).await.unwrap();
        assert_unique_iids(&art);
        let value = serde_json::to_value(&art).unwrap();
        assert_eq!(value["aid"], 4);
        let names: Vec<_> = value["services"][1]["characteristics"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|characteristic| characteristic["value"].as_str())
            .collect();
        assert_eq!(names, ["Art mode", "Art mode"]);
    }
}
