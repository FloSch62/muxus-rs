//! What a gNMI Set will leave behind, per change, so it can be shown as a
//! diff before it is sent: replace puts the value in place, update merges it
//! (list entries by their key), delete removes the path (port of
//! `client/src/management/set-review.ts`).

use muxus_core::gnmi_path::strip_module_prefix;
use muxus_core::js::js_trim;
use muxus_core::management::GnmiSetEncoding;
use serde_json::{Map, Value};

use super::data_tree::guess_list_keys;
use super::requests::{GnmiDraft, GnmiSetItem, GnmiSetItemOp};

/// The same leaf or container, whether or not the JSON qualified it with its module.
fn same_name(a: &str, b: &str) -> bool {
    a == b || strip_module_prefix(a) == strip_module_prefix(b)
}

fn merge_lists(current: &[Value], update: &[Value]) -> Vec<Value> {
    if !current.iter().all(Value::is_object) || !update.iter().all(Value::is_object) {
        return update.to_vec();
    }
    let all: Vec<&Map<String, Value>> = current
        .iter()
        .chain(update)
        .filter_map(Value::as_object)
        .collect();
    let keys = guess_list_keys(&all);
    if keys.is_empty() {
        return update.to_vec();
    }
    let identity = |entry: &Value| -> String {
        let entry = entry.as_object().expect("checked above");
        keys.iter()
            .map(|key| {
                entry
                    .iter()
                    .find(|(name, _)| same_name(name, key))
                    .map(|(_, value)| value.to_string())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("\u{0}")
    };
    let mut merged = current.to_vec();
    for entry in update {
        let wanted = identity(entry);
        match merged
            .iter()
            .position(|candidate| identity(candidate) == wanted)
        {
            Some(index) => merged[index] = merge_value(Some(&merged[index]), entry),
            None => merged.push(entry.clone()),
        }
    }
    merged
}

/// gNMI update semantics: objects merge, lists merge by key, scalars replace.
pub fn merge_value(current: Option<&Value>, update: &Value) -> Value {
    match (current, update) {
        (Some(Value::Object(current)), Value::Object(update)) => {
            let mut out = current.clone();
            for (name, value) in update {
                let existing = out
                    .keys()
                    .find(|candidate| same_name(candidate, name))
                    .cloned();
                match existing {
                    Some(existing) => {
                        let merged = merge_value(out.get(&existing), value);
                        out.insert(existing, merged);
                    }
                    None => {
                        out.insert(name.clone(), value.clone());
                    }
                }
            }
            Value::Object(out)
        }
        (Some(Value::Array(current)), Value::Array(update)) => {
            Value::Array(merge_lists(current, update))
        }
        _ => update.clone(),
    }
}

/// Stable key order, so a diff shows content changes and not ordering.
pub fn canonical_json(value: Option<&Value>) -> String {
    fn sort(input: &Value) -> Value {
        match input {
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            Value::Object(object) => {
                let mut keys: Vec<&String> = object.keys().collect();
                keys.sort_by(|a, b| strip_module_prefix(a).cmp(strip_module_prefix(b)));
                Value::Object(
                    keys.into_iter()
                        .map(|key| (key.clone(), sort(&object[key])))
                        .collect(),
                )
            }
            other => other.clone(),
        }
    }
    match value {
        None => String::new(),
        Some(value) => serde_json::to_string_pretty(&sort(value)).unwrap_or_default(),
    }
}

/// What the path holds once the change is applied; `None` for a delete.
pub fn proposed_value(
    item: &GnmiSetItem,
    current: Option<&Value>,
) -> Result<Option<Value>, String> {
    if item.op == GnmiSetItemOp::Delete {
        return Ok(None);
    }
    if item.encoding == GnmiSetEncoding::Ascii {
        return Ok(Some(Value::String(item.value.clone())));
    }
    let parsed: Value = serde_json::from_str(&item.value).map_err(|err| err.to_string())?;
    Ok(Some(if item.op == GnmiSetItemOp::Replace {
        parsed
    } else {
        merge_value(current, &parsed)
    }))
}

pub fn set_items(draft: &GnmiDraft) -> Vec<&GnmiSetItem> {
    draft
        .set
        .iter()
        .filter(|item| !js_trim(&item.path).is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn merges_updates_the_way_gnmi_does_lists_by_key() {
        assert_eq!(
            merge_value(
                Some(
                    &json!({"description": "a", "subinterface": [{"index": 0, "mtu": 1500}, {"index": 1}]})
                ),
                &json!({"srl_nokia-if:description": "b", "subinterface": [{"index": 0, "mtu": 9000}, {"index": 2}]}),
            ),
            json!({"description": "b", "subinterface": [{"index": 0, "mtu": 9000}, {"index": 1}, {"index": 2}]})
        );
        let item = GnmiSetItem {
            id: "1".into(),
            op: GnmiSetItemOp::Replace,
            path: "/a".into(),
            value: r#"{"x":1}"#.into(),
            encoding: GnmiSetEncoding::JsonIetf,
        };
        assert_eq!(
            proposed_value(&item, Some(&json!({"x": 0, "y": 1}))).unwrap(),
            Some(json!({"x": 1}))
        );
        let delete = GnmiSetItem {
            op: GnmiSetItemOp::Delete,
            ..item
        };
        assert_eq!(
            proposed_value(&delete, Some(&json!({"x": 0}))).unwrap(),
            None
        );
        assert_eq!(
            canonical_json(Some(&json!({"b": 1, "a": 2}))),
            "{\n  \"a\": 2,\n  \"b\": 1\n}"
        );
        assert_eq!(canonical_json(None), "");
    }
}
