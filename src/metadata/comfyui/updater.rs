use serde_json::Value;

pub fn update_metadata(value: &mut Value, field: &str, new_value: &str) -> bool {
    fn replace(target: &mut Value, new_value: &str) -> bool {
        let replacement = match target {
            Value::Number(_) => new_value
                .parse::<i64>()
                .ok()
                .map(|number| Value::Number(number.into()))
                .or_else(|| {
                    new_value
                        .parse::<f64>()
                        .ok()
                        .and_then(|number| serde_json::Number::from_f64(number).map(Value::Number))
                }),
            _ => Some(Value::String(new_value.to_string())),
        };
        let Some(replacement) = replacement else {
            return false;
        };
        if *target == replacement {
            return false;
        }
        *target = replacement;
        true
    }

    fn update_object(
        object: &mut serde_json::Map<String, Value>,
        field: &str,
        new_value: &str,
    ) -> bool {
        let class_type = object
            .get("class_type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let is_ace_step = class_type.contains("textencodeacestep")
            || class_type.contains("acestep") && class_type.contains("textencode");
        if is_ace_step {
            if let Some(inputs) = object.get_mut("inputs").and_then(Value::as_object_mut) {
                if let Some(target) = inputs
                    .iter_mut()
                    .find(|(key, _)| key.eq_ignore_ascii_case(field))
                    .map(|(_, value)| value)
                {
                    return replace(target, new_value);
                }
                if field == "key" {
                    if let Some(target) = inputs
                        .iter_mut()
                        .find(|(key, _)| key.eq_ignore_ascii_case("tonality"))
                        .map(|(_, value)| value)
                    {
                        return replace(target, new_value);
                    }
                }
            }
            if let Some(target) = object
                .iter_mut()
                .find(|(key, _)| key.eq_ignore_ascii_case(field))
                .map(|(_, value)| value)
            {
                return replace(target, new_value);
            }
        }

        let keys = object
            .get("inputs")
            .and_then(Value::as_array)
            .map(|inputs| {
                inputs
                    .iter()
                    .filter_map(|input| input.get("name").and_then(Value::as_str))
                    .map(str::to_ascii_lowercase)
                    .collect::<Vec<_>>()
            });
        if let Some(keys) = keys {
            let node_type = object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            let is_visual_ace_step = node_type.contains("textencodeacestep")
                || node_type.contains("acestep") && node_type.contains("textencode");
            if is_visual_ace_step {
                let uses_positional_metadata = object
                    .get("widgets_values")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values.len() >= 9
                            && !keys.iter().any(|key| key == "bpm" || key == "keyscale")
                    })
                    .unwrap_or(false);
                if uses_positional_metadata {
                    let widget_index = match field {
                        "prompt" => Some(0),
                        "lyrics" => Some(1),
                        "seed" => Some(2),
                        "bpm" => Some(4),
                        "key" => Some(8),
                        _ => None,
                    };
                    if let Some(target) = widget_index.and_then(|index| {
                        object
                            .get_mut("widgets_values")
                            .and_then(Value::as_array_mut)
                            .and_then(|values| values.get_mut(index))
                    }) {
                        return replace(target, new_value);
                    }
                }

                let mut widget_index = None;
                let mut value_index = 0;
                for (index, key) in keys.iter().enumerate() {
                    if key == field || field == "key" && (key == "keyscale" || key == "tonality") {
                        widget_index = Some(value_index);
                        break;
                    }
                    value_index += 1;
                    if key == "seed" {
                        value_index += 1;
                    }
                    if index + 1 == keys.len() {
                        break;
                    }
                }
                if let Some(widget_index) = widget_index {
                    if let Some(target) = object
                        .get_mut("widgets_values")
                        .and_then(Value::as_array_mut)
                        .and_then(|values| values.get_mut(widget_index))
                    {
                        return replace(target, new_value);
                    }
                }
            }
        }
        false
    }

    match value {
        Value::Object(object) => {
            let changed = update_object(object, field, new_value);
            if changed {
                return true;
            }
            object
                .values_mut()
                .any(|child| update_metadata(child, field, new_value))
        }
        Value::Array(array) => array
            .iter_mut()
            .any(|child| update_metadata(child, field, new_value)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn updates_api_metadata_without_touching_model_or_lora() {
        let mut value = json!({
            "1": {"class_type": "TextEncodeAceStepAudio1.5", "inputs": {
                "bpm": 128, "key": "C major", "seed": 31, "prompt": "old"
            }},
            "2": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "model.safetensors"}},
            "3": {"class_type": "Load LoRA", "inputs": {"lora_name": "style.safetensors"}}
        });
        assert!(super::update_metadata(&mut value, "bpm", "140"));
        assert!(super::update_metadata(&mut value, "prompt", "new"));
        assert_eq!(value["1"]["inputs"]["bpm"], json!(140));
        assert_eq!(value["1"]["inputs"]["prompt"], json!("new"));
        assert_eq!(
            value["2"]["inputs"]["ckpt_name"],
            json!("model.safetensors")
        );
        assert_eq!(
            value["3"]["inputs"]["lora_name"],
            json!("style.safetensors")
        );
    }

    #[test]
    fn updates_visual_metadata_after_seed_control_value() {
        let mut value = json!({"nodes": [{
            "type": "TextEncodeAceStepAudio1.5",
            "inputs": [
                {"name": "tags", "widget": {}},
                {"name": "lyrics", "widget": {}},
                {"name": "seed", "widget": {}},
                {"name": "bpm", "widget": {}},
                {"name": "keyscale", "widget": {}}
            ],
            "widgets_values": ["prompt", "lyrics", 31, "fixed", 130, "C major"]
        }]});
        assert!(super::update_metadata(&mut value, "bpm", "150"));
        assert!(super::update_metadata(&mut value, "key", "D minor"));
        assert!(super::update_metadata(&mut value, "lyrics", "new lyrics"));
        assert!(super::update_metadata(&mut value, "seed", "42"));
        assert_eq!(value["nodes"][0]["widgets_values"][2], json!(42));
        assert_eq!(value["nodes"][0]["widgets_values"][4], json!(150));
        assert_eq!(value["nodes"][0]["widgets_values"][5], json!("D minor"));
        assert_eq!(value["nodes"][0]["widgets_values"][1], json!("new lyrics"));
    }

    #[test]
    fn updates_visual_metadata_with_positional_prompt_and_lyrics() {
        let mut value = json!({"nodes": [{
            "type": "TextEncodeAceStepAudio1.5",
            "inputs": [
                {"name": "clip"},
                {"name": "seed", "widget": {"name": "seed"}},
                {"name": "duration", "widget": {"name": "duration"}}
            ],
            "widgets_values": [
                "prompt", "lyrics", 34, "fixed", 128, 30,
                "4", "en", "E minor", true, 2, 0.85, 0.9, 0, 0
            ]
        }]});

        assert!(super::update_metadata(&mut value, "seed", "777"));
        assert!(super::update_metadata(&mut value, "lyrics", "new lyrics"));
        assert_eq!(value["nodes"][0]["widgets_values"][1], json!("new lyrics"));
        assert_eq!(value["nodes"][0]["widgets_values"][2], json!(777));
    }
}
