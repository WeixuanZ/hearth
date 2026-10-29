//! The Frame, as one HomeKit television.
//!
//! Power is the television's own on and off. Art mode is a switch linked to it,
//! so it sits beside power rather than among the HDMI inputs. Brightness is the
//! art-mode brightness. Volume buttons send the TV's own volume keys, because
//! the set does not tell us the level. Navigation is Control Center's remote.

use super::protocol::Input;
use crate::{
    config::FrameDevice,
    hapset::{hook_bool, hook_i32, hook_u32, hook_u8, put},
};
use anyhow::{anyhow, Result};
use hap::{
    accessory::{AccessoryInformation, HapAccessory},
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
    art: SwitchService,
}

/// Service iid of the art switch, after the input slots (50 to 98).
const ART: u64 = 100;

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
        services.push(&self.art);
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
        services.push(&mut self.art);
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

pub async fn build(
    device: &FrameDevice,
    suppress: &Arc<AtomicBool>,
    tx: &mpsc::Sender<Command>,
) -> Result<FrameAccessory> {
    let model = if device.model.is_empty() {
        "The Frame 2024".into()
    } else {
        device.model.clone()
    };
    let info = AccessoryInformation {
        name: device.name.clone(),
        manufacturer: "Samsung".into(),
        model,
        serial_number: if device.mac.is_empty() {
            device.host.clone()
        } else {
            device.mac.clone()
        },
        firmware_revision: Some("2024".into()),
        ..Default::default()
    }
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
    set_text(&mut television.configured_name, &device.name).await?;
    if let Some(name) = television.name.as_mut() {
        set_text(name, &device.name).await?;
    }

    let mut speaker = SpeakerService::new(40, device.aid);
    speaker.volume = None;
    speaker.active = None;
    hook_bool(&mut speaker.mute, suppress, tx, |_| Command::Mute);
    if let Some(kind) = speaker.volume_control_type.as_mut() {
        kind.set_value(json!(1u8))
            .await
            .map_err(|error| anyhow!("{error}"))?;
    }
    if let Some(selector) = speaker.volume_selector.as_mut() {
        hook_u8(selector, suppress, tx, Command::Volume);
    }

    let mut next = 50;
    let mut inputs = Vec::new();
    for input in Input::ALL {
        let mut source = InputSourceService::new(next, device.aid);
        set_text(&mut source.configured_name, input.label()).await?;
        set_text(&mut source.name, input.label()).await?;
        source
            .input_source_type
            .set_value(json!(input.source_type()))
            .await
            .map_err(|error| anyhow!("{error}"))?;
        source
            .is_configured
            .set_value(json!(1u8))
            .await
            .map_err(|error| anyhow!("{error}"))?;
        source
            .current_visibility_state
            .set_value(json!(0u8))
            .await
            .map_err(|error| anyhow!("{error}"))?;
        if let Some(identifier) = source.identifier.as_mut() {
            identifier
                .set_value(json!(input as u32))
                .await
                .map_err(|error| anyhow!("{error}"))?;
        }
        if let Some(kind) = source.input_device_type.as_mut() {
            kind.set_value(json!(1u8))
                .await
                .map_err(|error| anyhow!("{error}"))?;
        }
        if let Some(target) = source.target_visibility_state.as_mut() {
            target
                .set_value(json!(0u8))
                .await
                .map_err(|error| anyhow!("{error}"))?;
        }
        next += 10;
        inputs.push(source);
    }
    let mut art = SwitchService::new(ART, device.aid);
    if let Some(name) = art.name.as_mut() {
        set_text(name, "Art mode").await?;
    }
    if let Some(name) = art.configured_name.as_mut() {
        set_text(name, "Art mode").await?;
    }
    hook_bool(&mut art.power_state, suppress, tx, Command::Art);

    let mut linked: Vec<u64> = inputs.iter().map(|source| source.get_id()).collect();
    linked.push(ART);
    television.set_linked_services(linked);

    Ok(FrameAccessory {
        id: device.aid,
        info,
        television,
        speaker,
        inputs,
        art,
    })
}

async fn set_text(characteristic: &mut impl HapCharacteristic, text: &str) -> Result<()> {
    characteristic
        .set_value(json!(text))
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
    put(
        accessory,
        HapType::Switch,
        HapType::PowerState,
        json!(view.art),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hapset::assert_unique_iids;

    #[tokio::test]
    async fn iids_are_unique() {
        let device = FrameDevice {
            aid: 2,
            name: "Television".into(),
            model: "QE00LS03D".into(),
            host: "10.0.0.2".into(),
            mac: "00:11:22:33:44:55".into(),
            token: String::new(),
        };
        let suppress = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel(4);
        let frame = build(&device, &suppress, &tx).await.unwrap();
        assert_unique_iids(&frame);

        let value = serde_json::to_value(&frame).unwrap();
        let services = value["services"].as_array().unwrap();
        let television = services
            .iter()
            .find(|service| service["iid"] == 20)
            .unwrap();
        assert_eq!(television["linked"], json!([50, 60, 70, 80, ART]));
        let art = services
            .iter()
            .find(|service| service["iid"] == ART)
            .unwrap();
        let name = art["characteristics"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|characteristic| characteristic["value"].as_str());
        assert_eq!(name, Some("Art mode"));
    }
}
