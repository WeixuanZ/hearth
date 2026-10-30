//! The feature list shipped in the appliance's description zip.
//!
//! UIDs differ from one machine to the next. The names do not, so the
//! description is read once, at pairing, and stored with the device.

use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use crate::config::BoschFeature;

pub fn features_from_zip(bytes: &[u8]) -> Result<BTreeMap<String, BoschFeature>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    let mut mapping = String::new();
    let mut description = String::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let name = file.name()?.to_ascii_lowercase();
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        if name.ends_with("featuremapping.xml") {
            mapping = text;
        } else if name.ends_with("devicedescription.xml") {
            description = text;
        }
    }
    if mapping.is_empty() {
        anyhow::bail!("the appliance description had no feature list");
    }
    let parsed = parse_features(&mapping);
    let enums = parse_enums(&mapping);
    let links = parse_links(&description);
    let mut features = BTreeMap::new();
    for (uid, name) in parsed {
        let mut feature = BoschFeature {
            name,
            values: BTreeMap::new(),
        };
        if let Some(enum_id) = links.get(&uid) {
            if let Some(values) = enums.get(enum_id) {
                feature.values.clone_from(values);
            }
        }
        features.insert(uid.to_string(), feature);
    }
    if features.is_empty() {
        anyhow::bail!("the appliance description listed no features");
    }
    Ok(features)
}

fn parse_features(xml: &str) -> BTreeMap<u32, String> {
    walk(xml, |frame| {
        frame.ref_uid.map(|uid| (uid, frame.text.trim().to_owned()))
    })
    .into_iter()
    .filter(|(_, name)| !name.is_empty())
    .collect()
}

fn parse_enums(xml: &str) -> BTreeMap<u32, BTreeMap<String, String>> {
    let mut enums: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
    let mut stack: Vec<Option<u32>> = Vec::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut pending: Option<(u32, String)> = None;
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(event)) => {
                let found = attrs(&event);
                let id = hex_attr(&found, "refENID");
                let current = id.or_else(|| stack.last().copied().flatten());
                pending = current.zip(found.get("refValue").cloned());
                stack.push(current);
            }
            Ok(Event::Empty(event)) => {
                let found = attrs(&event);
                let current =
                    hex_attr(&found, "refENID").or_else(|| stack.last().copied().flatten());
                if let (Some(id), Some(value)) = (current, found.get("refValue")) {
                    enums
                        .entry(id)
                        .or_default()
                        .entry(value.clone())
                        .or_insert_with(|| value.clone());
                }
            }
            Ok(Event::Text(event)) => {
                if let Some((id, value)) = pending.clone() {
                    let text = xml_text(&event);
                    if !text.is_empty() {
                        enums.entry(id).or_default().insert(value, text);
                        pending = None;
                    }
                }
            }
            Ok(Event::End(_)) => {
                pending = None;
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    enums
}

fn parse_links(xml: &str) -> BTreeMap<u32, u32> {
    if xml.is_empty() {
        return BTreeMap::new();
    }
    let mut links = BTreeMap::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event)) => {
                let attrs = attrs(&event);
                if let (Some(uid), Some(enum_id)) =
                    (hex_attr(&attrs, "uid"), hex_attr(&attrs, "enumerationType"))
                {
                    links.insert(uid, enum_id);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    links
}

struct Open {
    ref_uid: Option<u32>,
    text: String,
}

fn walk(xml: &str, mut take: impl FnMut(&Open) -> Option<(u32, String)>) -> Vec<(u32, String)> {
    let mut found = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(event)) => {
                let attrs = attrs(&event);
                stack.push(Open {
                    ref_uid: hex_attr(&attrs, "refUID"),
                    text: String::new(),
                });
            }
            Ok(Event::Empty(event)) => {
                let attrs = attrs(&event);
                let open = Open {
                    ref_uid: hex_attr(&attrs, "refUID"),
                    text: String::new(),
                };
                if let Some(pair) = take(&open) {
                    found.push(pair);
                }
            }
            Ok(Event::Text(event)) => {
                if let Some(open) = stack.last_mut() {
                    open.text.push_str(&xml_text(&event));
                }
            }
            Ok(Event::End(_)) => {
                if let Some(open) = stack.pop() {
                    if let Some(pair) = take(&open) {
                        found.push(pair);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    found
}

fn attrs(event: &quick_xml::events::BytesStart<'_>) -> BTreeMap<String, String> {
    event
        .attributes()
        .flatten()
        .filter_map(|attr| {
            let key = attr.key.local_name().into_inner();
            let key = key.rsplit('}').next().unwrap_or(key).to_owned();
            let value = attr
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()?
                .into_owned();
            Some((key, value))
        })
        .collect()
}

fn hex_attr(attrs: &BTreeMap<String, String>, key: &str) -> Option<u32> {
    let text = attrs.get(key)?;
    u32::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

fn xml_text(event: &quick_xml::events::BytesText<'_>) -> String {
    let raw = event.xml10_content();
    match quick_xml::escape::unescape(raw.as_ref()) {
        Ok(text) => text.trim().to_owned(),
        Err(_) => raw.trim().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feature_picks_up_its_enum_from_the_description() {
        let mapping = r#"
            <map>
              <features>
                <feature refUID="C8">BSH.Common.Setting.PowerState</feature>
              </features>
              <enums>
                <enum refENID="A" enumKey="OnOff">
                  <value refValue="0">Off</value>
                  <value refValue="1">On</value>
                </enum>
              </enums>
            </map>"#;
        let description = r#"<desc><setting uid="C8" enumerationType="A"/></desc>"#;
        let features = parse_features(mapping);
        let enums = parse_enums(mapping);
        let links = parse_links(description);
        assert_eq!(
            features.get(&0xC8).map(String::as_str),
            Some("BSH.Common.Setting.PowerState")
        );
        assert_eq!(
            enums.get(&0xA).unwrap().get("1").map(String::as_str),
            Some("On")
        );
        assert_eq!(links.get(&0xC8), Some(&0xA));
    }

    #[test]
    fn the_description_zip_joins_a_feature_to_its_values() {
        let mapping = r#"
            <map>
              <feature refUID="C8">BSH.Common.Setting.PowerState</feature>
              <enum refENID="A">
                <value refValue="0">Off</value>
                <value refValue="1">On</value>
              </enum>
            </map>"#;
        let description = r#"<desc><setting uid="C8" enumerationType="A"/></desc>"#;
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut archive = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default();
            archive
                .start_file("SPV2HKX42G_FeatureMapping.xml", options)
                .unwrap();
            std::io::Write::write_all(&mut archive, mapping.as_bytes()).unwrap();
            archive
                .start_file("SPV2HKX42G_DeviceDescription.xml", options)
                .unwrap();
            std::io::Write::write_all(&mut archive, description.as_bytes()).unwrap();
            archive.finish().unwrap();
        }
        let features = features_from_zip(cursor.get_ref()).unwrap();
        let power = features.get("200").unwrap();
        assert_eq!(power.name, "BSH.Common.Setting.PowerState");
        assert_eq!(power.values.get("1").map(String::as_str), Some("On"));
    }
}
