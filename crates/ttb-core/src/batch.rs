use crate::models::ExpectedFields;
use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestProduct {
    pub product_id: String,
    pub label: Option<String>,
    pub image_names: Vec<String>,
    pub expected: ExpectedFields,
}

pub fn parse_manifest(filename: Option<&str>, bytes: &[u8]) -> Result<Vec<ManifestProduct>> {
    let text = std::str::from_utf8(bytes).context("manifest must be UTF-8")?;
    let text = text.trim_start_matches('\u{feff}');
    let trimmed = text.trim_start();
    let looks_json = filename.is_some_and(|name| name.to_lowercase().ends_with(".json"))
        || trimmed.starts_with('[')
        || trimmed.starts_with('{');

    if looks_json {
        parse_json_manifest(text)
    } else {
        parse_csv_manifest(text)
    }
}

fn parse_json_manifest(text: &str) -> Result<Vec<ManifestProduct>> {
    let value: Value = serde_json::from_str(text).context("parse JSON manifest")?;
    let products = match value {
        Value::Array(items) => items,
        Value::Object(mut obj) => obj
            .remove("products")
            .and_then(|value| value.as_array().cloned())
            .ok_or_else(|| anyhow!("JSON manifest must be an array or contain a products array"))?,
        _ => return Err(anyhow!("JSON manifest must be an array")),
    };

    products
        .into_iter()
        .enumerate()
        .map(|(idx, item)| {
            let object = item
                .as_object()
                .ok_or_else(|| anyhow!("manifest product at index {idx} is not an object"))?;
            let map = normalize_json_object(object);
            let images = image_names_from_json_object(object)
                .or_else(|| image_names_from_map(&map))
                .ok_or_else(|| anyhow!("manifest product at index {idx} is missing images"))?;
            let product_id = get_alias(&map, &["productid", "product", "ttbid", "id"])
                .unwrap_or_else(|| {
                    images
                        .first()
                        .cloned()
                        .unwrap_or_else(|| format!("product-{idx}"))
                });

            Ok(ManifestProduct {
                label: get_alias(&map, &["label", "productname", "name", "product"]),
                product_id,
                image_names: images,
                expected: expected_from_map(&map),
            })
        })
        .collect()
}

fn parse_csv_manifest(text: &str) -> Result<Vec<ManifestProduct>> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(text.as_bytes());
    let headers = reader
        .headers()
        .context("read CSV manifest headers")?
        .iter()
        .map(normalize_key)
        .collect::<Vec<_>>();

    let mut products = Vec::new();
    for (idx, record) in reader.records().enumerate() {
        let record = record.with_context(|| format!("read CSV manifest row {}", idx + 1))?;
        let mut map = HashMap::new();
        for (header, value) in headers.iter().zip(record.iter()) {
            if !value.trim().is_empty() {
                map.insert(header.clone(), value.trim().to_string());
            }
        }

        let filename = get_alias(&map, &["filename", "image", "imagefile", "file"])
            .ok_or_else(|| anyhow!("CSV row {} is missing filename/image", idx + 1))?;
        let product_id = get_alias(&map, &["productid", "product", "ttbid", "id"])
            .unwrap_or_else(|| filename.clone());

        products.push(ManifestProduct {
            label: get_alias(&map, &["label", "productname", "name", "product"]),
            product_id,
            image_names: vec![filename],
            expected: expected_from_map(&map),
        });
    }

    Ok(products)
}

fn normalize_json_object(object: &serde_json::Map<String, Value>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (key, value) in object {
        let key = normalize_key(key);
        match value {
            Value::String(text) => {
                out.insert(key, text.clone());
            }
            Value::Number(number) => {
                out.insert(key, number.to_string());
            }
            Value::Array(items) => {
                let joined = items
                    .iter()
                    .filter_map(|item| {
                        item.as_str()
                            .map(ToOwned::to_owned)
                            .or_else(|| image_name_from_json_value(item))
                    })
                    .collect::<Vec<_>>()
                    .join("|");
                out.insert(key, joined);
            }
            _ => {}
        }
    }
    out
}

fn image_names_from_map(map: &HashMap<String, String>) -> Option<Vec<String>> {
    get_alias(
        map,
        &[
            "images",
            "image",
            "filename",
            "file",
            "files",
            "imagenames",
            "frontimage",
            "backimage",
        ],
    )
    .map(|value| {
        value
            .split(['|', ',', ';'])
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    })
}

fn image_names_from_json_object(object: &serde_json::Map<String, Value>) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for alias in [
        "images",
        "image",
        "filename",
        "file",
        "files",
        "image_names",
        "imageNames",
        "front_image",
        "frontImage",
        "back_image",
        "backImage",
    ] {
        if let Some(value) = object.get(alias) {
            collect_image_names(value, &mut names);
        }
    }

    if names.is_empty() { None } else { Some(names) }
}

fn collect_image_names(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            names.extend(
                text.split(['|', ',', ';'])
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned),
            );
        }
        Value::Array(items) => {
            for item in items {
                collect_image_names(item, names);
            }
        }
        Value::Object(_) => {
            if let Some(name) = image_name_from_json_value(value) {
                names.push(name);
            }
        }
        _ => {}
    }
}

fn image_name_from_json_value(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    [
        "image",
        "filename",
        "file",
        "path",
        "name",
        "front_image",
        "back_image",
    ]
    .iter()
    .find_map(|key| object.get(*key).and_then(Value::as_str))
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(ToOwned::to_owned)
}

fn expected_from_map(map: &HashMap<String, String>) -> ExpectedFields {
    ExpectedFields {
        brand_name: get_alias(map, &["brandname", "brand"]),
        class_type: get_alias(map, &["classtype", "class", "type", "designation"]),
        alcohol_content: get_alias(map, &["alcoholcontent", "abv", "alcohol", "alcvol"]),
        net_contents: get_alias(map, &["netcontents", "netcontent", "contents", "volume"]),
        bottler: get_alias(map, &["bottler", "producer", "nameandaddress"]),
        country: get_alias(map, &["country", "countryoforigin", "origin"]),
    }
}

fn get_alias(map: &HashMap<String, String>, aliases: &[&str]) -> Option<String> {
    aliases
        .iter()
        .find_map(|alias| map.get(*alias).cloned())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_csv_manifest() {
        let csv = "filename,brand_name,class_type,alcohol_content,net_contents\nfront.png,Old Tom,Bourbon,45%,750 mL\n";
        let parsed = parse_manifest(Some("manifest.csv"), csv.as_bytes()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].image_names, vec!["front.png"]);
        assert_eq!(parsed[0].expected.brand_name.as_deref(), Some("Old Tom"));
    }

    #[test]
    fn parses_json_multi_image_manifest() {
        let json = r#"[{"product":"Old Tom 750","images":["front.jpg","back.jpg"],"brand":"Old Tom","abv":"45%","net_contents":"750 mL"}]"#;
        let parsed = parse_manifest(Some("manifest.json"), json.as_bytes()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].image_names, vec!["front.jpg", "back.jpg"]);
        assert_eq!(parsed[0].expected.alcohol_content.as_deref(), Some("45%"));
    }

    #[test]
    fn parses_json_products_object_with_image_aliases_and_metadata() {
        let json = r#"{
            "products": [{
                "product_id": "hard-one",
                "image_names": ["folder/front.png", {"filename": "back.png"}],
                "brand_name": "Night Orchard",
                "expected_verdict": "pass_or_review",
                "expected_path": "enhanced_retry",
                "notes": "human-only metadata"
            }]
        }"#;
        let parsed = parse_manifest(Some("manifest.json"), json.as_bytes()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].product_id, "hard-one");
        assert_eq!(parsed[0].image_names, vec!["folder/front.png", "back.png"]);
        assert_eq!(
            parsed[0].expected.brand_name.as_deref(),
            Some("Night Orchard")
        );
    }

    #[test]
    fn parses_json_front_and_back_image_fields() {
        let json = r#"[{
            "id": "multi",
            "front_image": "front.jpg",
            "back_image": {"file": "back.jpg"},
            "class_type": "Red Wine"
        }]"#;
        let parsed = parse_manifest(Some("manifest.json"), json.as_bytes()).unwrap();
        assert_eq!(parsed[0].image_names, vec!["front.jpg", "back.jpg"]);
        assert_eq!(parsed[0].expected.class_type.as_deref(), Some("Red Wine"));
    }

    #[test]
    fn parses_json_manifest_with_utf8_bom() {
        let json =
            "\u{feff}{\"products\":[{\"id\":\"bom\",\"image\":\"front.png\",\"brand\":\"Brand\"}]}";
        let parsed = parse_manifest(Some("manifest.json"), json.as_bytes()).unwrap();
        assert_eq!(parsed[0].product_id, "bom");
        assert_eq!(parsed[0].image_names, vec!["front.png"]);
    }
}
