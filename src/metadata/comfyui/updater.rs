use serde_json::{json, Value};

fn is_lora_node(node: &Value) -> bool {
    node.get("class_type")
        .or_else(|| node.get("type"))
        .and_then(Value::as_str)
        .map(|class_type| {
            let class_type = class_type.to_ascii_lowercase();
            class_type.contains("loraloader") || class_type.contains("loadlora")
        })
        .unwrap_or(false)
}

fn node_inputs(node: &Value) -> Option<&serde_json::Map<String, Value>> {
    node.get("inputs").and_then(Value::as_object)
}

fn node_inputs_mut(node: &mut Value) -> Option<&mut serde_json::Map<String, Value>> {
    node.get_mut("inputs").and_then(Value::as_object_mut)
}

fn find_node_mut<'a>(value: &'a mut Value, node_id: &str) -> Option<&'a mut Value> {
    let is_target = value.get("id").and_then(|id| {
        id.as_i64()
            .map(|id| id.to_string())
            .or_else(|| id.as_str().map(str::to_string))
    }) == Some(node_id.to_string());
    if is_target {
        return Some(value);
    }
    match value {
        Value::Object(object) => {
            if object.get("class_type").is_some() {
                return None;
            }
            if object.get(node_id).is_some_and(Value::is_object) {
                return object.get_mut(node_id);
            }
            object
                .values_mut()
                .find_map(|child| find_node_mut(child, node_id))
        }
        Value::Array(array) => array
            .iter_mut()
            .find_map(|child| find_node_mut(child, node_id)),
        _ => None,
    }
}

fn node_ids(value: &Value) -> Vec<String> {
    let Value::Object(object) = value else {
        return Vec::new();
    };
    if let Some(nodes) = object.get("nodes").and_then(Value::as_array) {
        return nodes
            .iter()
            .filter(|node| is_lora_node(node))
            .filter_map(|node| {
                node.get("id").and_then(|id| {
                    id.as_i64()
                        .map(|id| id.to_string())
                        .or_else(|| id.as_str().map(str::to_string))
                })
            })
            .collect();
    }
    object
        .iter()
        .filter_map(|(id, node)| is_lora_node(node).then(|| id.clone()))
        .collect()
}

fn reference_id(value: &Value) -> Option<String> {
    value
        .as_array()
        .and_then(|reference| reference.first())
        .and_then(|id| {
            id.as_str()
                .map(str::to_string)
                .or_else(|| id.as_i64().map(|id| id.to_string()))
        })
}

fn model_reference(node: &Value) -> Option<String> {
    node_inputs(node)?.iter().find_map(|(key, value)| {
        (key.eq_ignore_ascii_case("model") || key.to_ascii_lowercase().starts_with("model"))
            .then(|| reference_id(value))
            .flatten()
    })
}

fn set_model_reference(node: &mut Value, node_id: &str) -> bool {
    let Some(inputs) = node_inputs_mut(node) else {
        return false;
    };
    let Some((_, reference)) = inputs.iter_mut().find(|(key, value)| {
        (key.eq_ignore_ascii_case("model") || key.to_ascii_lowercase().starts_with("model"))
            && value.is_array()
    }) else {
        return false;
    };
    *reference = serde_json::json!([node_id, 0]);
    true
}

fn set_lora_input(node: &mut Value, key_name: &str, value: Value) -> bool {
    let Some(inputs) = node_inputs_mut(node) else {
        return false;
    };
    let Some((_, target)) = inputs
        .iter_mut()
        .find(|(key, _)| key.eq_ignore_ascii_case(key_name))
    else {
        return false;
    };
    *target = value;
    true
}

pub fn set_lora_path(value: &mut Value, node_id: &str, path: &str) -> bool {
    let Some(node) = find_node_mut(value, node_id) else {
        return false;
    };
    let replacement = Value::String(path.to_string());
    if let Some(values) = node.get_mut("widgets_values").and_then(Value::as_array_mut) {
        let Some(target) = values.first_mut() else {
            return false;
        };
        if *target == replacement {
            return false;
        }
        *target = replacement;
        return true;
    }
    let Some(inputs) = node_inputs_mut(node) else {
        return false;
    };
    let Some((_, target)) = inputs.iter_mut().find(|(key, _)| {
        key.eq_ignore_ascii_case("lora_name")
            || key.eq_ignore_ascii_case("filename")
            || key.eq_ignore_ascii_case("file_name")
            || key.eq_ignore_ascii_case("name")
    }) else {
        return false;
    };
    if *target == replacement {
        return false;
    }
    *target = replacement;
    true
}

pub fn add_lora(value: &mut Value, filename: &str) -> Option<String> {
    let ids = node_ids(value);
    let template_id = ids.last()?;
    let template = find_node_mut(value, template_id)?.clone();
    if value.get("nodes").is_some() {
        return add_visual_lora(value, &ids, &template, filename);
    }
    let object = value.as_object_mut()?;
    let mut new_id = 1;
    while object.contains_key(&new_id.to_string()) {
        new_id += 1;
    }
    let new_id = new_id.to_string();
    let mut node = template;
    let filename = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    if !set_lora_input(&mut node, "lora_name", Value::String(filename.to_string())) {
        return None;
    }
    if !set_lora_input(&mut node, "strength_model", serde_json::json!(1.0)) {
        set_lora_input(&mut node, "strength", serde_json::json!(1.0));
    }
    object.insert(new_id.clone(), node);
    let mut ordered_ids = ids;
    ordered_ids.push(new_id.clone());
    reorder_loras(value, &ordered_ids).then_some(new_id)
}

fn add_visual_lora(
    value: &mut Value,
    ids: &[String],
    template: &Value,
    filename: &str,
) -> Option<String> {
    let object = value.as_object_mut()?;
    let new_id = object
        .get("nodes")?
        .as_array()?
        .iter()
        .filter_map(|node| node.get("id").and_then(Value::as_i64))
        .max()
        .unwrap_or(0)
        + 1;
    let last_id = ids.last()?.parse::<i64>().ok()?;
    let filename = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    let mut node = template.clone();
    node["id"] = serde_json::json!(new_id);
    if let Some(position) = object
        .get("nodes")
        .and_then(Value::as_array)
        .and_then(|nodes| {
            nodes
                .iter()
                .find(|candidate| candidate.get("id") == Some(&serde_json::json!(last_id)))
        })
        .and_then(|node| node.get("pos"))
        .and_then(Value::as_array)
    {
        if let (Some(x), Some(y)) = (
            position.first().and_then(Value::as_f64),
            position.get(1).and_then(Value::as_f64),
        ) {
            node["pos"] = serde_json::json!([x + 10.0, y + 10.0]);
        }
    }
    if let Some(values) = node.get_mut("widgets_values").and_then(Value::as_array_mut) {
        if let Some(target) = values.get_mut(0) {
            *target = Value::String(filename.to_string());
        }
        if let Some(target) = values.get_mut(1) {
            *target = serde_json::json!(1.0);
        }
    } else {
        return None;
    }
    let old_link = object
        .get("nodes")?
        .as_array()?
        .iter()
        .find(|candidate| candidate.get("id") == Some(&serde_json::json!(last_id)))
        .and_then(|candidate| candidate.get("outputs"))
        .and_then(Value::as_array)
        .and_then(|outputs| outputs.first())
        .and_then(|output| output.get("links"))
        .and_then(Value::as_array)
        .and_then(|links| links.first())
        .and_then(Value::as_i64)?;
    let new_link = object
        .get("last_link_id")
        .and_then(Value::as_i64)
        .unwrap_or(old_link)
        + 1;
    object.insert("last_node_id".into(), serde_json::json!(new_id));
    object.insert("last_link_id".into(), serde_json::json!(new_link));
    let link_type = object
        .get_mut("links")
        .and_then(Value::as_array_mut)
        .and_then(|links| {
            let link = links
                .iter_mut()
                .find(|link| link.get(0) == Some(&serde_json::json!(old_link)))?;
            let link_type = link.get(5)?.clone();
            link[1] = serde_json::json!(new_id);
            Some(link_type)
        })?;
    if let Some(inputs) = node.get_mut("inputs").and_then(Value::as_array_mut) {
        if let Some(model) = inputs.iter_mut().find(|input| {
            input
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case("model"))
        }) {
            model["link"] = serde_json::json!(new_link);
        }
    }
    if let Some(outputs) = node.get_mut("outputs").and_then(Value::as_array_mut) {
        if let Some(links) = outputs
            .first_mut()
            .and_then(|output| output.get_mut("links"))
            .and_then(Value::as_array_mut)
        {
            links[0] = serde_json::json!(old_link);
        }
    }
    if let Some(outputs) = object
        .get_mut("nodes")
        .and_then(Value::as_array_mut)
        .and_then(|nodes| {
            nodes
                .iter_mut()
                .find(|candidate| candidate.get("id") == Some(&serde_json::json!(last_id)))
        })
        .and_then(|candidate| candidate.get_mut("outputs"))
        .and_then(Value::as_array_mut)
    {
        if let Some(links) = outputs
            .first_mut()
            .and_then(|output| output.get_mut("links"))
            .and_then(Value::as_array_mut)
        {
            links[0] = serde_json::json!(new_link);
        }
    }
    object
        .get_mut("links")
        .and_then(Value::as_array_mut)?
        .push(serde_json::json!([
            new_link, last_id, 0, new_id, 0, link_type
        ]));
    object.get_mut("nodes")?.as_array_mut()?.push(node);
    Some(new_id.to_string())
}

pub fn set_lora_strength(value: &mut Value, node_id: &str, strength: &str) -> bool {
    let Some(node) = find_node_mut(value, node_id) else {
        return false;
    };
    let Ok(number) = strength.parse::<f64>() else {
        return false;
    };
    let Some(number) = serde_json::Number::from_f64(number) else {
        return false;
    };
    let replacement = Value::Number(number);
    if let Some(values) = node.get_mut("widgets_values").and_then(Value::as_array_mut) {
        let Some(target) = values.get_mut(1) else {
            return false;
        };
        if *target == replacement {
            return false;
        }
        *target = replacement;
        return true;
    }
    let Some(inputs) = node_inputs_mut(node) else {
        return false;
    };
    let Some((_, target)) = inputs.iter_mut().find(|(key, _)| {
        key.eq_ignore_ascii_case("strength_model")
            || key.eq_ignore_ascii_case("strength")
            || key.eq_ignore_ascii_case("weight")
    }) else {
        return false;
    };
    if *target == replacement {
        return false;
    }
    *target = replacement;
    true
}

pub fn reorder_loras(value: &mut Value, ordered_ids: &[String]) -> bool {
    let current_ids = node_ids(value);
    if ordered_ids.len() != current_ids.len()
        || ordered_ids.iter().any(|id| !current_ids.contains(id))
    {
        return false;
    }
    let original_predecessor = current_ids
        .iter()
        .filter_map(|id| find_node_mut(value, id).and_then(|node| model_reference(node)))
        .find(|id| !current_ids.contains(id));
    let Some(original_predecessor) = original_predecessor else {
        return false;
    };
    for id in &current_ids {
        let Some(node) = find_node_mut(value, id) else {
            return false;
        };
        if !is_lora_node(node) {
            return false;
        }
    }
    for (index, id) in ordered_ids.iter().enumerate() {
        let predecessor = if index == 0 {
            original_predecessor.as_str()
        } else {
            ordered_ids[index - 1].as_str()
        };
        let Some(node) = find_node_mut(value, id) else {
            return false;
        };
        set_model_reference(node, predecessor);
    }
    true
}

pub fn remove_lora(value: &mut Value, node_id: &str) -> bool {
    if value.get("nodes").is_some() {
        return remove_visual_lora(value, node_id);
    }
    let mut ids = node_ids(value);
    if !ids.iter().any(|id| id == node_id) {
        return false;
    }
    let predecessor = find_node_mut(value, node_id).and_then(|node| model_reference(node));
    let successor = ids.iter().find_map(|id| {
        (id != node_id)
            .then(|| find_node_mut(value, id).and_then(|node| model_reference(node)))
            .flatten()
            .filter(|id| id == node_id)
            .map(|_| id.clone())
    });
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    object.remove(node_id);
    ids.retain(|id| id != node_id);
    if ids.is_empty() {
        return true;
    }
    if let (Some(predecessor), Some(successor)) = (predecessor, successor) {
        if let Some(node) = find_node_mut(value, &successor) {
            set_model_reference(node, &predecessor);
        }
    }
    reorder_loras(value, &ids)
}

fn remove_visual_lora(value: &mut Value, node_id: &str) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    let Some(nodes) = object.get("nodes").and_then(Value::as_array) else {
        return false;
    };
    let Some(node_index) = nodes.iter().position(|node| {
        node.get("id")
            .and_then(Value::as_i64)
            .is_some_and(|id| id.to_string() == node_id)
    }) else {
        return false;
    };
    let node = &nodes[node_index];
    let incoming_link = node
        .get("inputs")
        .and_then(Value::as_array)
        .and_then(|inputs| {
            inputs.iter().find_map(|input| {
                input
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| name.eq_ignore_ascii_case("model"))
                    .and_then(|_| input.get("link"))
                    .and_then(Value::as_i64)
            })
        });
    let outgoing_link = node
        .get("outputs")
        .and_then(Value::as_array)
        .and_then(|outputs| outputs.first())
        .and_then(|output| output.get("links"))
        .and_then(Value::as_array)
        .and_then(|links| links.first())
        .and_then(Value::as_i64);

    let Some(incoming_link) = incoming_link else {
        return false;
    };
    let successor_id = outgoing_link.and_then(|link_id| {
        object
            .get("links")
            .and_then(Value::as_array)
            .and_then(|links| {
                links
                    .iter()
                    .find(|link| link.get(0) == Some(&json!(link_id)))
            })
            .and_then(|link| link.get(3))
            .and_then(Value::as_i64)
    });

    if let Some(successor_id) = successor_id {
        let Some(successor) = object
            .get_mut("nodes")
            .and_then(Value::as_array_mut)
            .and_then(|nodes| {
                nodes
                    .iter_mut()
                    .find(|candidate| candidate.get("id") == Some(&json!(successor_id)))
            })
        else {
            return false;
        };
        let Some(model_input) = successor
            .get_mut("inputs")
            .and_then(Value::as_array_mut)
            .and_then(|inputs| {
                inputs.iter_mut().find(|input| {
                    input
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| name.eq_ignore_ascii_case("model"))
                })
            })
        else {
            return false;
        };
        model_input["link"] = json!(incoming_link);
        if let Some(links) = object.get_mut("links").and_then(Value::as_array_mut) {
            if let Some(link) = links
                .iter_mut()
                .find(|link| link.get(0) == Some(&json!(incoming_link)))
            {
                link[3] = json!(successor_id);
            }
            if let Some(outgoing_link) = outgoing_link {
                links.retain(|link| link.get(0) != Some(&json!(outgoing_link)));
            }
        }
    } else {
        let Some(links) = object.get_mut("links").and_then(Value::as_array_mut) else {
            return false;
        };
        let predecessor_id = links
            .iter()
            .find(|link| link.get(0) == Some(&json!(incoming_link)))
            .and_then(|link| link.get(1))
            .and_then(Value::as_i64);
        links.retain(|link| link.get(0) != Some(&json!(incoming_link)));
        if let Some(predecessor_id) = predecessor_id {
            if let Some(predecessor) = object
                .get_mut("nodes")
                .and_then(Value::as_array_mut)
                .and_then(|nodes| {
                    nodes
                        .iter_mut()
                        .find(|node| node.get("id") == Some(&json!(predecessor_id)))
                })
            {
                if let Some(links) = predecessor
                    .get_mut("outputs")
                    .and_then(Value::as_array_mut)
                    .and_then(|outputs| outputs.first_mut())
                    .and_then(|output| output.get_mut("links"))
                    .and_then(Value::as_array_mut)
                {
                    links.retain(|link| link.as_i64() != Some(incoming_link));
                }
            }
        }
    }

    object
        .get_mut("nodes")
        .and_then(Value::as_array_mut)
        .map(|nodes| nodes.remove(node_index));
    true
}

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
        let is_ksampler = class_type.contains("ksampler");
        let is_model_loader = class_type.contains("checkpointloader")
            || class_type.contains("unetloader")
            || class_type.contains("model loader");
        let is_load_audio = class_type
            .replace([' ', '_', '-'], "")
            .contains("loadaudio");
        let requested_field = match field {
            "ksampler_cfg" => "cfg",
            "ksampler_steps" => "steps",
            _ => field,
        };
        if is_ksampler || is_model_loader || is_load_audio {
            if let Some(inputs) = object.get_mut("inputs").and_then(Value::as_object_mut) {
                let target_keys: &[&str] = if is_ksampler {
                    &["cfg", "steps"]
                } else if is_model_loader {
                    &[
                        "ckpt_name",
                        "unet_name",
                        "model_name",
                        "checkpoint",
                        "checkpoint_name",
                    ]
                } else {
                    &["audio", "audio_name", "filename", "file_name", "name"]
                };
                if let Some(target) = inputs
                    .iter_mut()
                    .find(|(key, _)| {
                        (field == "model"
                            && is_model_loader
                            && target_keys
                                .iter()
                                .any(|candidate| key.eq_ignore_ascii_case(candidate)))
                            || (field == "reference_audio"
                                && is_load_audio
                                && target_keys
                                    .iter()
                                    .any(|candidate| key.eq_ignore_ascii_case(candidate)))
                            || target_keys.iter().any(|candidate| {
                                requested_field.eq_ignore_ascii_case(candidate)
                                    && key.eq_ignore_ascii_case(candidate)
                            })
                    })
                    .map(|(_, value)| value)
                {
                    return replace(target, new_value);
                }
            }
        }
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
            let node_name = object
                .get("properties")
                .and_then(Value::as_object)
                .and_then(|properties| properties.get("Node name for S&R"))
                .map(|value| value.to_string().to_ascii_lowercase())
                .unwrap_or_default();
            let node_kind = format!("{node_type} {node_name}");
            let is_visual_ace_step = node_kind.contains("textencodeacestep")
                || node_type.contains("acestep") && node_type.contains("textencode");
            let is_visual_ksampler = node_kind.contains("ksampler");
            let is_visual_model_loader = node_kind.contains("checkpointloader")
                || node_kind.contains("unetloader")
                || node_kind.contains("model loader");
            let is_visual_load_audio = node_kind.replace([' ', '_', '-'], "").contains("loadaudio");
            if is_visual_ksampler || is_visual_model_loader || is_visual_load_audio {
                let target_names: &[&str] = if is_visual_ksampler {
                    &["cfg", "steps"]
                } else if is_visual_model_loader {
                    &[
                        "ckpt_name",
                        "unet_name",
                        "model_name",
                        "checkpoint",
                        "checkpoint_name",
                    ]
                } else {
                    &["audio", "audio_name", "filename", "file_name", "name"]
                };
                let target_index = keys.iter().enumerate().find_map(|(index, key)| {
                    let matches = (field == "model"
                        && is_visual_model_loader
                        && target_names.iter().any(|name| key == name))
                        || (field == "reference_audio"
                            && is_visual_load_audio
                            && target_names.iter().any(|name| key == name))
                        || target_names
                            .iter()
                            .any(|name| requested_field == *name && key == name);
                    matches.then_some(index)
                });
                if let Some(target_index) = target_index {
                    let widget_index = object
                        .get("inputs")
                        .and_then(Value::as_array)
                        .and_then(|inputs| {
                            let mut widget_index = 0;
                            inputs.iter().find_map(|input| {
                                let name = input.get("name").and_then(Value::as_str)?;
                                let is_target = name.eq_ignore_ascii_case(&keys[target_index]);
                                if input.get("widget").is_some() {
                                    let current = widget_index;
                                    widget_index += 1;
                                    if name.eq_ignore_ascii_case("seed") {
                                        widget_index += 1;
                                    }
                                    return is_target.then_some(current);
                                }
                                None
                            })
                        })
                        .or_else(|| (is_visual_model_loader || is_visual_load_audio).then_some(0));
                    if let Some(target) = widget_index.and_then(|index| {
                        object
                            .get_mut("widgets_values")
                            .and_then(Value::as_array_mut)
                            .and_then(|values| values.get_mut(index))
                    }) {
                        return replace(target, new_value);
                    }
                }
            }
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
                        "duration" => Some(5),
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
    fn adds_lora_to_visual_model_chain() {
        let mut value = json!({
            "last_node_id": 106,
            "last_link_id": 292,
            "nodes": [
                {
                    "id": 106,
                    "type": "LoraLoaderModelOnly",
                    "pos": [100, 200],
                    "inputs": [{"name": "model", "link": 291}],
                    "outputs": [{"name": "MODEL", "links": [292]}],
                    "widgets_values": ["existing.safetensors", 1.2]
                },
                {
                    "id": 78,
                    "type": "ModelSamplingAuraFlow",
                    "inputs": [{"name": "model", "link": 292}]
                }
            ],
            "links": [[292, 106, 0, 78, 0, "MODEL"]]
        });

        let node_id = super::add_lora(&mut value, "/models/new.safetensors").unwrap();
        assert_eq!(node_id, "107");
        assert_eq!(value["nodes"][2]["pos"], json!([110.0, 210.0]));
        assert_eq!(value["nodes"][0]["outputs"][0]["links"], json!([293]));
        assert_eq!(value["nodes"][1]["inputs"][0]["link"], json!(292));
        assert_eq!(
            value["nodes"][2]["widgets_values"],
            json!(["new.safetensors", 1.0])
        );
        assert_eq!(value["nodes"][2]["inputs"][0]["link"], json!(293));
        assert_eq!(value["links"][0], json!([292, 107, 0, 78, 0, "MODEL"]));
        assert_eq!(value["links"][1], json!([293, 106, 0, 107, 0, "MODEL"]));
    }

    #[test]
    fn removes_lora_from_visual_model_chain() {
        let mut value = json!({
            "nodes": [
                {"id": 1, "type": "CheckpointLoaderSimple", "outputs": [{"links": [10]}]},
                {"id": 2, "type": "LoraLoaderModelOnly", "inputs": [{"name": "model", "link": 10}], "outputs": [{"links": [11]}]},
                {"id": 3, "type": "LoraLoaderModelOnly", "inputs": [{"name": "model", "link": 11}], "outputs": [{"links": [12]}]},
                {"id": 4, "type": "ModelSamplingAuraFlow", "inputs": [{"name": "model", "link": 12}]}
            ],
            "links": [
                [10, 1, 0, 2, 0, "MODEL"],
                [11, 2, 0, 3, 0, "MODEL"],
                [12, 3, 0, 4, 0, "MODEL"]
            ]
        });

        assert!(super::remove_lora(&mut value, "2"));
        assert_eq!(value["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(value["nodes"][1]["inputs"][0]["link"], json!(10));
        assert_eq!(
            value["links"],
            json!([[10, 1, 0, 3, 0, "MODEL"], [12, 3, 0, 4, 0, "MODEL"]])
        );
    }

    #[test]
    fn removes_last_lora_and_its_predecessor_link() {
        let mut value = json!({
            "nodes": [
                {"id": 1, "type": "CheckpointLoaderSimple", "outputs": [{"links": [10]}]},
                {"id": 2, "type": "LoraLoaderModelOnly", "inputs": [{"name": "model", "link": 10}], "outputs": [{"links": []}]}
            ],
            "links": [[10, 1, 0, 2, 0, "MODEL"]]
        });

        assert!(super::remove_lora(&mut value, "2"));
        assert_eq!(value["nodes"][0]["outputs"][0]["links"], json!([]));
        assert_eq!(value["links"], json!([]));
    }

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
    fn updates_ksampler_and_reference_audio_metadata() {
        let mut value = json!({
            "1": {"class_type": "KSampler", "inputs": {"steps": 8, "cfg": 1.0}},
            "2": {"class_type": "LoadAudio", "inputs": {"audio": "old.flac"}}
        });

        assert!(super::update_metadata(&mut value, "ksampler_steps", "12"));
        assert!(super::update_metadata(&mut value, "ksampler_cfg", "2.5"));
        assert!(super::update_metadata(
            &mut value,
            "reference_audio",
            "reference.flac"
        ));
        assert_eq!(value["1"]["inputs"]["steps"], json!(12));
        assert_eq!(value["1"]["inputs"]["cfg"], json!(2.5));
        assert_eq!(value["2"]["inputs"]["audio"], json!("reference.flac"));
    }

    #[test]
    fn sets_custom_path_in_api_lora_node() {
        let mut value = json!({
            "3": {"class_type": "Load LoRA", "inputs": {
                "lora_name": "style.safetensors"
            }}
        });

        assert!(super::set_lora_path(
            &mut value,
            "3",
            "subfolder/style.safetensors"
        ));
        assert_eq!(
            value["3"]["inputs"]["lora_name"],
            json!("subfolder/style.safetensors")
        );
    }

    #[test]
    fn sets_custom_path_in_visual_lora_node() {
        let mut value = json!({
            "nodes": [{
                "id": 7,
                "type": "LoraLoaderModelOnly",
                "widgets_values": ["style.safetensors", 1.0]
            }]
        });

        assert!(super::set_lora_path(
            &mut value,
            "7",
            "/models/loras/style.safetensors"
        ));
        assert_eq!(
            value["nodes"][0]["widgets_values"][0],
            json!("/models/loras/style.safetensors")
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
        assert!(super::update_metadata(&mut value, "duration", "45"));
        assert!(super::update_metadata(&mut value, "lyrics", "new lyrics"));
        assert_eq!(value["nodes"][0]["widgets_values"][1], json!("new lyrics"));
        assert_eq!(value["nodes"][0]["widgets_values"][2], json!(777));
        assert_eq!(value["nodes"][0]["widgets_values"][5], json!(45));
    }
}
