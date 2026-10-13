//! Leaf values as the device sent them (`PathText.tsx`: `valueText`,
//! `valueTone`).

use std::sync::LazyLock;

use muxus_mgmt::workbench::data_tree::js_number_text;
use regex::Regex;
use serde_json::Value;

/// A leaf value as text, the way the device sent it.
pub fn value_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| value_text(Some(item)))
            .collect::<Vec<_>>()
            .join(", "),
        Some(object @ Value::Object(_)) => object.to_string(),
        Some(Value::Number(number)) => js_number_text(number),
        Some(Value::Bool(flag)) => flag.to_string(),
    }
}

/// How a leaf value reads: a number, or a state that reads as good or bad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueTone {
    Success,
    Error,
    Info,
}

static NUMERIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-?\d+(\.\d+)?$").expect("valid regex"));
static GOOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(up|enable|enabled|active|established|true|ok|running|ready)$")
        .expect("valid regex")
});
static BAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(down|disable|disabled|failed|error|idle|false|lower-layer-down|not-present)$",
    )
    .expect("valid regex")
});

/// A hue for leaf values by what they look like: numbers, and states that read as good or bad.
pub fn value_tone(value: Option<&Value>) -> Option<ValueTone> {
    match value {
        Some(Value::Number(_)) => Some(ValueTone::Info),
        Some(Value::String(text)) if NUMERIC.is_match(text) => Some(ValueTone::Info),
        Some(Value::String(text)) if GOOD.is_match(text) => Some(ValueTone::Success),
        Some(Value::String(text)) if BAD.is_match(text) => Some(ValueTone::Error),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_values_like_javascript() {
        assert_eq!(value_text(None), "");
        assert_eq!(value_text(Some(&Value::Null)), "");
        assert_eq!(value_text(Some(&json!("up"))), "up");
        assert_eq!(value_text(Some(&json!(1.5))), "1.5");
        assert_eq!(value_text(Some(&json!(3))), "3");
        assert_eq!(value_text(Some(&json!(true))), "true");
        assert_eq!(value_text(Some(&json!(["a", 1, null]))), "a, 1, ");
        assert_eq!(value_text(Some(&json!({"a": 1}))), r#"{"a":1}"#);
    }

    #[test]
    fn tones_numbers_and_states() {
        assert_eq!(value_tone(Some(&json!(7))), Some(ValueTone::Info));
        assert_eq!(value_tone(Some(&json!("-12.5"))), Some(ValueTone::Info));
        assert_eq!(value_tone(Some(&json!("UP"))), Some(ValueTone::Success));
        assert_eq!(
            value_tone(Some(&json!("established"))),
            Some(ValueTone::Success)
        );
        assert_eq!(
            value_tone(Some(&json!("lower-layer-down"))),
            Some(ValueTone::Error)
        );
        assert_eq!(value_tone(Some(&json!("ethernet-1/1"))), None);
        assert_eq!(value_tone(Some(&json!(true))), None);
        assert_eq!(value_tone(None), None);
    }
}
