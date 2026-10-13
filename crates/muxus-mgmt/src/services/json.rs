//! The JSON form of protobuf messages that travels between the UI and the
//! backend for gNOI and gNSI: proto field names, enums by name, bytes as
//! base64, 64-bit integers as numbers while exact and decimal strings
//! beyond. Well-known types (Timestamp, Any, …) stay plain messages, as the
//! TypeScript has them.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use muxus_core::js::{js_number, js_trim};
use muxus_core::management::GrpcJson;
use prost_reflect::{
    DynamicMessage, FieldDescriptor, Kind, MessageDescriptor, ReflectMessage, Value as ProtoValue,
};
use serde_json::Value;

use crate::error::ProtoJsonError;
use crate::gnmi::values::int64_to_json;
use crate::proto::is_map_entry;

fn error(message: String) -> ProtoJsonError {
    ProtoJsonError(message)
}

fn is_map_field(field: &FieldDescriptor) -> Option<MessageDescriptor> {
    match field.kind() {
        Kind::Message(entry) if field.is_list() && is_map_entry(entry.full_name()) => Some(entry),
        _ => None,
    }
}

/// `JSON.stringify` of a value, for error messages.
fn shown(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// `Number(value)` in JavaScript, for the JSON a form sends.
fn js_to_number(value: &Value) -> f64 {
    match value {
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::Bool(flag) => f64::from(u8::from(*flag)),
        Value::String(text) => {
            let text = js_trim(text);
            if text.is_empty() {
                return 0.0;
            }
            text.parse().unwrap_or(f64::NAN)
        }
        Value::Null => 0.0,
        _ => f64::NAN,
    }
}

fn integer(field: &FieldDescriptor, value: &Value) -> Result<i128, ProtoJsonError> {
    let not_integer = || {
        error(format!(
            "{}: {} is not an integer",
            field.name(),
            shown(value)
        ))
    };
    match value {
        Value::Number(number) => number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from))
            .or_else(|| {
                number
                    .as_f64()
                    .filter(|value| value.fract() == 0.0 && value.abs() < 1.8e19)
                    .map(|value| value as i128)
            })
            .ok_or_else(not_integer),
        Value::String(text) => {
            let text = js_trim(text);
            let digits = text.strip_prefix('-').unwrap_or(text);
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return Err(not_integer());
            }
            text.parse::<i128>().map_err(|_| not_integer())
        }
        _ => Err(not_integer()),
    }
}

/// Base64 the way Node's `Buffer.from(text, 'base64')` reads it: URL-safe
/// letters accepted, padding and anything outside the alphabet ignored.
fn lenient_base64(text: &str) -> Vec<u8> {
    let cleaned: String = text
        .chars()
        .filter_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' => Some(c),
            '-' => Some('+'),
            '_' => Some('/'),
            _ => None,
        })
        .collect();
    let usable = cleaned.len() - cleaned.len() % 4
        + if cleaned.len() % 4 >= 2 {
            cleaned.len() % 4
        } else {
            0
        };
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(&cleaned[..usable])
        .unwrap_or_default()
}

fn scalar_to_proto(
    field: &FieldDescriptor,
    kind: &Kind,
    value: &Value,
) -> Result<ProtoValue, ProtoJsonError> {
    let name = field.name();
    Ok(match kind {
        Kind::String => match value {
            Value::String(text) => ProtoValue::String(text.clone()),
            _ => return Err(error(format!("{name} must be text"))),
        },
        Kind::Bytes => match value {
            Value::String(text) => ProtoValue::Bytes(Bytes::from(lenient_base64(text))),
            _ => return Err(error(format!("{name} must be base64 text"))),
        },
        Kind::Bool => ProtoValue::Bool(value == &Value::Bool(true)),
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
            ProtoValue::I64(integer(field, value)? as i64)
        }
        Kind::Uint64 | Kind::Fixed64 => ProtoValue::U64(integer(field, value)? as u64),
        Kind::Enum(descriptor) => {
            let number = match value {
                Value::Number(number) if number.is_i64() => {
                    number.as_i64().map(|value| value as i32)
                }
                Value::String(text) => descriptor
                    .get_value_by_name(text)
                    .map(|value| value.number()),
                _ => None,
            };
            ProtoValue::EnumNumber(
                number.ok_or_else(|| {
                    error(format!("{name}: {} is not a valid value", shown(value)))
                })?,
            )
        }
        Kind::Double
        | Kind::Float
        | Kind::Int32
        | Kind::Sint32
        | Kind::Sfixed32
        | Kind::Uint32
        | Kind::Fixed32 => {
            let number = js_to_number(value);
            if !number.is_finite() {
                return Err(error(format!("{name} must be a number")));
            }
            match kind {
                Kind::Double => ProtoValue::F64(number),
                Kind::Float => ProtoValue::F32(number as f32),
                Kind::Uint32 | Kind::Fixed32 => ProtoValue::U32(number.trunc() as i64 as u32),
                _ => ProtoValue::I32(number.trunc() as i64 as i32),
            }
        }
        Kind::Message(_) => unreachable!("messages are converted by json_to_proto"),
    })
}

fn convert_item(field: &FieldDescriptor, item: &Value) -> Result<ProtoValue, ProtoJsonError> {
    match field.kind() {
        Kind::Message(message) => Ok(ProtoValue::Message(json_to_proto(&message, item)?)),
        kind => scalar_to_proto(field, &kind, item),
    }
}

/// JSON from the UI → the message to encode. Unknown keys are refused.
pub fn json_to_proto(
    desc: &MessageDescriptor,
    value: &Value,
) -> Result<DynamicMessage, ProtoJsonError> {
    let mut message = DynamicMessage::new(desc.clone());
    let input = match value {
        Value::Null => return Ok(message),
        Value::Object(object) => object,
        _ => return Err(error(format!("{} must be an object", desc.full_name()))),
    };
    for key in input.keys() {
        if desc.get_field_by_name(key).is_none() {
            return Err(error(format!("{} has no field {key}", desc.full_name())));
        }
    }
    for field in desc.fields() {
        let Some(raw) = input.get(field.name()).filter(|raw| !raw.is_null()) else {
            continue;
        };
        if let Some(entry) = is_map_field(&field) {
            let Value::Object(object) = raw else {
                return Err(error(format!("{} must be an object", field.name())));
            };
            let key_field = entry.get_field(1).expect("map entries have a key");
            let value_field = entry.get_field(2).expect("map entries have a value");
            let mut entries = Vec::new();
            for (key, item) in object {
                let mut pair = DynamicMessage::new(entry.clone());
                pair.set_field(
                    &key_field,
                    scalar_to_proto(&key_field, &key_field.kind(), &Value::String(key.clone()))?,
                );
                let item = match (&value_field.kind(), item) {
                    // Map values are written as text, as `String(entry)` does.
                    (Kind::String, Value::String(_)) => item.clone(),
                    (Kind::String, other) => Value::String(match other {
                        Value::Number(number) => {
                            crate::workbench::data_tree::js_number_text(number)
                        }
                        other => other.to_string(),
                    }),
                    _ => item.clone(),
                };
                pair.set_field(&value_field, convert_item(&value_field, &item)?);
                entries.push(ProtoValue::Message(pair));
            }
            message.set_field(&field, ProtoValue::List(entries));
            continue;
        }
        if field.is_list() {
            let Value::Array(items) = raw else {
                return Err(error(format!("{} must be a list", field.name())));
            };
            let items = items
                .iter()
                .map(|item| convert_item(&field, item))
                .collect::<Result<Vec<_>, _>>()?;
            message.set_field(&field, ProtoValue::List(items));
            continue;
        }
        message.set_field(&field, convert_item(&field, raw)?);
    }
    Ok(message)
}

fn scalar_to_json(field: &FieldDescriptor, value: &ProtoValue) -> Value {
    match value {
        ProtoValue::Bool(flag) => Value::Bool(*flag),
        ProtoValue::I32(number) => Value::from(*number),
        ProtoValue::U32(number) => Value::from(*number),
        ProtoValue::I64(number) => int64_to_json(*number),
        ProtoValue::U64(number) => int64_to_json(*number),
        ProtoValue::F32(number) => js_number(f64::from(*number)),
        ProtoValue::F64(number) => js_number(*number),
        ProtoValue::String(text) => Value::String(text.clone()),
        ProtoValue::Bytes(bytes) => Value::String(BASE64.encode(bytes)),
        ProtoValue::EnumNumber(number) => match field.kind() {
            Kind::Enum(descriptor) => descriptor.get_value(*number).map_or_else(
                || Value::from(*number),
                |value| Value::String(value.name().to_owned()),
            ),
            _ => Value::from(*number),
        },
        ProtoValue::Message(message) => Value::Object(proto_to_json(message)),
        ProtoValue::List(items) => Value::Array(
            items
                .iter()
                .map(|item| scalar_to_json(field, item))
                .collect(),
        ),
        ProtoValue::Map(_) => Value::Null,
    }
}

/// A decoded message → JSON for the UI.
pub fn proto_to_json(message: &DynamicMessage) -> GrpcJson {
    let mut out = GrpcJson::new();
    for field in message.descriptor().fields() {
        if !message.has_field(&field) {
            continue;
        }
        let value = message.get_field(&field);
        if is_map_field(&field).is_some() {
            let ProtoValue::List(entries) = value.as_ref() else {
                continue;
            };
            let mut object = GrpcJson::new();
            for entry in entries {
                let ProtoValue::Message(pair) = entry else {
                    continue;
                };
                let key = match pair.get_field_by_number(1).as_deref() {
                    Some(ProtoValue::String(key)) => key.clone(),
                    Some(other) => scalar_to_json(&field, other).to_string(),
                    None => String::new(),
                };
                let value_field = pair
                    .descriptor()
                    .get_field(2)
                    .expect("map entries have a value");
                let item = pair.get_field(&value_field);
                object.insert(key, scalar_to_json(&value_field, &item));
            }
            out.insert(field.name().to_owned(), Value::Object(object));
            continue;
        }
        out.insert(field.name().to_owned(), scalar_to_json(&field, &value));
    }
    out
}

/// Encode JSON as the message `desc` describes.
pub fn encode_json(desc: &MessageDescriptor, value: &Value) -> Result<Vec<u8>, ProtoJsonError> {
    use prost::Message;
    Ok(json_to_proto(desc, value)?.encode_to_vec())
}

/// Decode a message `desc` describes as JSON.
pub fn decode_json(desc: &MessageDescriptor, bytes: &[u8]) -> Result<GrpcJson, String> {
    DynamicMessage::decode(desc.clone(), bytes)
        .map(|message| proto_to_json(&message))
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use muxus_core::management::grpc_methods;
    use serde_json::json;

    use super::*;
    use crate::proto::pool;
    use crate::services::registry::method_def;

    #[test]
    fn turns_ui_json_into_messages_and_back() {
        let ping = method_def(grpc_methods::PING).unwrap();
        let message = json_to_proto(
            &ping.request,
            &json!({
                "destination": "10.0.0.2",
                "count": 3,
                "interval": "1000000000",
                "l3protocol": "IPV4",
                "network_instance": "mgmt",
                "do_not_resolve": true
            }),
        )
        .unwrap();
        assert_eq!(
            message.get_field_by_name("interval").unwrap().as_ref(),
            &ProtoValue::I64(1_000_000_000)
        );
        assert_eq!(
            message.get_field_by_name("l3protocol").unwrap().as_ref(),
            &ProtoValue::EnumNumber(1)
        );
        let bytes = encode_json(&ping.request, &Value::Object(proto_to_json(&message))).unwrap();
        let decoded = decode_json(&ping.request, &bytes).unwrap();
        assert_eq!(decoded["destination"], "10.0.0.2");
        assert_eq!(decoded["interval"], 1_000_000_000);
        assert_eq!(decoded["l3protocol"], "IPV4");
        assert_eq!(decoded["do_not_resolve"], true);
        assert_eq!(decoded["count"], 3);
    }

    #[test]
    fn keeps_64_bit_values_exact_and_bytes_as_base64() {
        let time = method_def(grpc_methods::TIME).unwrap();
        let mut response = DynamicMessage::new(time.response.clone());
        response.set_field_by_name("time", ProtoValue::U64(1_760_000_000_123_456_789));
        assert_eq!(proto_to_json(&response)["time"], "1760000000123456789");
        let get_response = pool().get_message_by_name("gnoi.file.GetResponse").unwrap();
        let mut file = DynamicMessage::new(get_response);
        file.set_field_by_name("contents", ProtoValue::Bytes(Bytes::from_static(b"hello")));
        assert_eq!(proto_to_json(&file)["contents"], BASE64.encode("hello"));
    }

    #[test]
    fn refuses_fields_and_enum_values_the_message_does_not_have() {
        let ping = method_def(grpc_methods::PING).unwrap();
        assert!(json_to_proto(&ping.request, &json!({"destination": "x", "ttl": 4})).is_err());
        assert!(
            json_to_proto(&ping.request, &json!({"l3protocol": "IPX"}))
                .unwrap_err()
                .0
                .contains("not a valid value")
        );
        assert!(
            json_to_proto(&ping.request, &json!({"interval": "1s"}))
                .unwrap_err()
                .0
                .contains("not an integer")
        );
        assert!(json_to_proto(&ping.request, &json!([1])).is_err());
        assert!(json_to_proto(&ping.request, &Value::Null).is_ok());
    }

    #[test]
    fn writes_maps_as_objects_in_their_order_and_timestamps_as_plain_messages() {
        let types_path = pool().get_message_by_name("gnoi.types.Path").unwrap();
        let path = json!({"elem": [{"name": "components"}, {"name": "component", "key": {"name": "lc1", "slot": "1"}}]});
        let message = json_to_proto(&types_path, &path).unwrap();
        assert_eq!(Value::Object(proto_to_json(&message)), path);
        let healthz = pool()
            .get_message_by_name("gnoi.healthz.ComponentStatus")
            .unwrap();
        let status = json!({"id": "e1", "created": {"seconds": 1_760_000_000, "nanos": 5}});
        let message = json_to_proto(&healthz, &status).unwrap();
        assert_eq!(Value::Object(proto_to_json(&message)), status);
        assert_eq!(lenient_base64("aGVsbG8"), b"hello");
        assert_eq!(lenient_base64("aGVs bG8="), b"hello");
    }
}
