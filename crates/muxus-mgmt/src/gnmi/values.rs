//! Conversions between gNMI protobuf messages and the JSON the UI sees.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use muxus_core::gnmi_path::{
    GnmiKeys, GnmiPath, GnmiPathElem, GnmiPathError, format_gnmi_path, join_gnmi_paths,
    parse_gnmi_path,
};
use muxus_core::js::js_number;
use muxus_core::management::{
    GnmiEncoding, GnmiModel, GnmiNotification, GnmiSetEncoding, GnmiUpdate, GnmiValue,
    GnmiValueType,
};
use serde_json::Value;

use crate::proto::gnmi::{self, typed_value};

/// gNMI's Encoding enum.
pub fn encoding_number(encoding: GnmiEncoding) -> i32 {
    match encoding {
        GnmiEncoding::Json => 0,
        GnmiEncoding::Bytes => 1,
        GnmiEncoding::Proto => 2,
        GnmiEncoding::Ascii => 3,
        GnmiEncoding::JsonIetf => 4,
    }
}

fn encoding_from_number(number: i32) -> Option<GnmiEncoding> {
    Some(match number {
        0 => GnmiEncoding::Json,
        1 => GnmiEncoding::Bytes,
        2 => GnmiEncoding::Proto,
        3 => GnmiEncoding::Ascii,
        4 => GnmiEncoding::JsonIetf,
        _ => return None,
    })
}

pub fn path_to_proto(path: &GnmiPath) -> gnmi::Path {
    gnmi::Path {
        origin: path.origin.clone().unwrap_or_default(),
        elem: path
            .elems
            .iter()
            .map(|elem| gnmi::PathElem {
                name: elem.name.clone(),
                key: elem
                    .keys
                    .as_ref()
                    .map(|keys| crate::proto::path_keys(keys.iter()))
                    .unwrap_or_default(),
            })
            .collect(),
        ..Default::default()
    }
}

pub fn path_text_to_proto(text: &str) -> Result<gnmi::Path, GnmiPathError> {
    Ok(path_to_proto(&parse_gnmi_path(text)?))
}

#[allow(deprecated)]
pub fn path_from_proto(message: Option<&gnmi::Path>) -> GnmiPath {
    let Some(message) = message else {
        return GnmiPath::default();
    };
    let elems = if !message.elem.is_empty() {
        message
            .elem
            .iter()
            .map(|elem| GnmiPathElem {
                name: elem.name.clone(),
                keys: (!elem.key.is_empty()).then(|| {
                    elem.key
                        .iter()
                        .map(|entry| (entry.key.as_str(), entry.value.as_str()))
                        .collect::<GnmiKeys>()
                }),
            })
            .collect()
    } else {
        // Pre-0.4 targets send plain string elements.
        message
            .element
            .iter()
            .map(|name| GnmiPathElem::new(name.clone()))
            .collect()
    };
    GnmiPath {
        origin: (!message.origin.is_empty()).then(|| message.origin.clone()),
        elems,
    }
}

/// Largest integer a JavaScript number holds exactly.
const MAX_SAFE_INTEGER: i128 = 9_007_199_254_740_991;

/// A 64-bit integer as a number while it is exact, as a decimal string beyond.
pub fn int64_to_json(value: impl Into<i128>) -> Value {
    let value = value.into();
    if value.abs() <= MAX_SAFE_INTEGER {
        Value::from(value as i64)
    } else {
        Value::String(value.to_string())
    }
}

fn parse_json(bytes: &[u8]) -> Value {
    let text = String::from_utf8_lossy(bytes);
    serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.into_owned()))
}

fn value(kind: GnmiValueType, value: Value) -> GnmiValue {
    GnmiValue { kind, value }
}

fn none() -> GnmiValue {
    value(GnmiValueType::None, Value::Null)
}

#[allow(deprecated)]
pub fn decode_typed_value(typed: Option<&gnmi::TypedValue>) -> GnmiValue {
    let Some(inner) = typed.and_then(|typed| typed.value.as_ref()) else {
        return none();
    };
    match inner {
        typed_value::Value::JsonIetfVal(bytes) => value(GnmiValueType::JsonIetf, parse_json(bytes)),
        typed_value::Value::JsonVal(bytes) => value(GnmiValueType::Json, parse_json(bytes)),
        typed_value::Value::StringVal(text) => {
            value(GnmiValueType::String, Value::from(text.as_str()))
        }
        typed_value::Value::IntVal(number) => value(GnmiValueType::Int, int64_to_json(*number)),
        typed_value::Value::UintVal(number) => value(GnmiValueType::Uint, int64_to_json(*number)),
        typed_value::Value::BoolVal(flag) => value(GnmiValueType::Bool, Value::Bool(*flag)),
        typed_value::Value::DoubleVal(number) => value(GnmiValueType::Double, js_number(*number)),
        typed_value::Value::FloatVal(number) => {
            value(GnmiValueType::Float, js_number(f64::from(*number)))
        }
        typed_value::Value::AsciiVal(text) => {
            value(GnmiValueType::Ascii, Value::from(text.as_str()))
        }
        typed_value::Value::BytesVal(bytes) => {
            value(GnmiValueType::Bytes, Value::from(BASE64.encode(bytes)))
        }
        typed_value::Value::ProtoBytes(bytes) => {
            value(GnmiValueType::Proto, Value::from(BASE64.encode(bytes)))
        }
        typed_value::Value::DecimalVal(decimal) => value(
            GnmiValueType::Decimal,
            decimal_to_json(decimal.digits, decimal.precision),
        ),
        typed_value::Value::LeaflistVal(list) => value(
            GnmiValueType::Leaflist,
            Value::Array(
                list.element
                    .iter()
                    .map(|element| decode_typed_value(Some(element)).value)
                    .collect(),
            ),
        ),
        typed_value::Value::AnyVal(any) => {
            let mut object = serde_json::Map::new();
            object.insert("typeUrl".into(), Value::from(any.type_url.as_str()));
            object.insert("value".into(), Value::from(BASE64.encode(&any.value)));
            value(GnmiValueType::Any, Value::Object(object))
        }
    }
}

/// `digits × 10^-precision` as the JavaScript number the TypeScript makes of it.
fn decimal_to_json(digits: i64, precision: u32) -> Value {
    let negative = digits < 0;
    let precision = precision as usize;
    let text = format!("{:0>width$}", digits.unsigned_abs(), width = precision + 1);
    let whole = if precision > 0 {
        let split = text.len() - precision;
        format!("{}.{}", &text[..split], &text[split..])
    } else {
        text
    };
    let parsed: f64 = format!("{}{whole}", if negative { "-" } else { "" })
        .parse()
        .unwrap_or(f64::NAN);
    js_number(parsed)
}

/// The deprecated Update.value of pre-TypedValue targets.
fn decode_legacy_value(legacy: &gnmi::Value) -> GnmiValue {
    let json = encoding_number(GnmiEncoding::Json);
    let json_ietf = encoding_number(GnmiEncoding::JsonIetf);
    if legacy.r#type == json || legacy.r#type == json_ietf {
        let kind = if legacy.r#type == json {
            GnmiValueType::Json
        } else {
            GnmiValueType::JsonIetf
        };
        return value(kind, parse_json(&legacy.value));
    }
    if legacy.r#type == encoding_number(GnmiEncoding::Ascii) {
        return value(
            GnmiValueType::Ascii,
            Value::from(String::from_utf8_lossy(&legacy.value).into_owned()),
        );
    }
    value(
        GnmiValueType::Bytes,
        Value::from(BASE64.encode(&legacy.value)),
    )
}

#[allow(deprecated)]
pub fn notification_to_json(message: &gnmi::Notification) -> GnmiNotification {
    let prefix = message
        .prefix
        .as_ref()
        .map(|prefix| path_from_proto(Some(prefix)));
    let absolute = |path: Option<&gnmi::Path>| {
        format_gnmi_path(&join_gnmi_paths(prefix.as_ref(), &path_from_proto(path)))
    };
    let updates = message
        .update
        .iter()
        .map(|update| GnmiUpdate {
            path: absolute(update.path.as_ref()),
            value: if update.val.is_some() {
                decode_typed_value(update.val.as_ref())
            } else if let Some(legacy) = &update.value {
                decode_legacy_value(legacy)
            } else {
                none()
            },
            duplicates: (update.duplicates != 0).then_some(u64::from(update.duplicates)),
        })
        .collect();
    GnmiNotification {
        timestamp: message.timestamp.to_string(),
        prefix: prefix
            .as_ref()
            .filter(|prefix| !prefix.elems.is_empty() || prefix.origin.is_some())
            .map(format_gnmi_path),
        atomic: message.atomic.then_some(true),
        updates,
        deletes: message
            .delete
            .iter()
            .map(|path| absolute(Some(path)))
            .collect(),
    }
}

/// Standard encodings the device lists; vendor-specific numbers are left out.
pub fn encodings_from_proto(values: &[i32]) -> Vec<GnmiEncoding> {
    let mut names = Vec::new();
    for value in values {
        if let Some(name) = encoding_from_number(*value)
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names
}

pub fn models_from_proto(values: &[gnmi::ModelData]) -> Vec<GnmiModel> {
    values
        .iter()
        .map(|model| GnmiModel {
            name: model.name.clone(),
            organization: model.organization.clone(),
            version: model.version.clone(),
        })
        .collect()
}

/// The encoding to request: the profile's choice, else the best one offered.
pub fn pick_encoding(offered: &[GnmiEncoding], preferred: Option<GnmiEncoding>) -> GnmiEncoding {
    if let Some(preferred) = preferred {
        return preferred;
    }
    for candidate in [GnmiEncoding::JsonIetf, GnmiEncoding::Json] {
        if offered.contains(&candidate) {
            return candidate;
        }
    }
    // A device that lists nothing is assumed to speak the default, JSON.
    offered.first().copied().unwrap_or(GnmiEncoding::Json)
}

/// A Set value as a TypedValue.
pub fn set_value_to_proto(text: &str, encoding: GnmiSetEncoding) -> gnmi::TypedValue {
    let value = match encoding {
        GnmiSetEncoding::Ascii => typed_value::Value::AsciiVal(text.to_owned()),
        GnmiSetEncoding::Json => typed_value::Value::JsonVal(text.as_bytes().to_vec()),
        GnmiSetEncoding::JsonIetf => typed_value::Value::JsonIetfVal(text.as_bytes().to_vec()),
    };
    gnmi::TypedValue { value: Some(value) }
}

#[cfg(test)]
mod tests {
    use prost::Message;
    use serde_json::json;

    use super::*;
    use crate::proto::gnmi_ext;

    fn path(text: &str) -> gnmi::Path {
        path_text_to_proto(text).unwrap()
    }

    fn typed(value: typed_value::Value) -> gnmi::TypedValue {
        gnmi::TypedValue { value: Some(value) }
    }

    #[test]
    fn encodes_a_get_request_with_keyed_paths_and_the_depth_extension_and_reads_it_back() {
        let request = gnmi::GetRequest {
            prefix: Some(path("/network-instance[name=default]")),
            path: vec![path("/protocols/bgp/neighbor[peer-address=10.0.0.1]")],
            r#type: 2,
            encoding: 4,
            extension: vec![gnmi_ext::Extension {
                ext: Some(gnmi_ext::extension::Ext::Depth(gnmi_ext::Depth {
                    level: 2,
                })),
            }],
            ..Default::default()
        };
        let decoded = gnmi::GetRequest::decode(request.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded, request);
        assert_eq!(
            format_gnmi_path(&path_from_proto(decoded.path.first())),
            "/protocols/bgp/neighbor[peer-address=10.0.0.1]"
        );
    }

    #[test]
    fn keeps_64_bit_integers_exact_including_negatives_and_values_past_2_53() {
        let notification = gnmi::Notification {
            timestamp: 1_791_624_701_947_184_204,
            update: vec![
                gnmi::Update {
                    path: Some(path("/a")),
                    val: Some(typed(typed_value::Value::UintVal(u64::MAX))),
                    ..Default::default()
                },
                gnmi::Update {
                    path: Some(path("/b")),
                    val: Some(typed(typed_value::Value::IntVal(-42))),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let decoded = gnmi::Notification::decode(notification.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded.timestamp, 1_791_624_701_947_184_204);
        let json = notification_to_json(&decoded);
        assert_eq!(json.timestamp, "1791624701947184204");
        let values: Vec<_> = json
            .updates
            .iter()
            .map(|update| update.value.clone())
            .collect();
        assert_eq!(
            values,
            vec![
                value(GnmiValueType::Uint, json!("18446744073709551615")),
                value(GnmiValueType::Int, json!(-42)),
            ]
        );
    }

    #[test]
    fn decodes_packed_repeated_enums_and_skips_unknown_fields() {
        // supported_encodings packed (field 2): [0, 4, 52], then an unknown field 9 (varint 1).
        let mut bytes = vec![0x12, 0x03, 0x00, 0x04, 0x34, 0x48, 0x01, 0x1a, 0x06];
        bytes.extend_from_slice(b"0.10.0");
        let decoded = gnmi::CapabilityResponse::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.supported_encodings, vec![0, 4, 52]);
        assert_eq!(decoded.g_nmi_version, "0.10.0");
        // Vendor-specific encoding numbers are left out of the list the UI sees.
        assert_eq!(
            encodings_from_proto(&decoded.supported_encodings),
            vec![GnmiEncoding::Json, GnmiEncoding::JsonIetf]
        );
    }

    #[test]
    fn encodes_poll_as_an_empty_message_and_subscription_intervals_in_nanoseconds() {
        let poll = gnmi::SubscribeRequest {
            request: Some(gnmi::subscribe_request::Request::Poll(gnmi::Poll {})),
            ..Default::default()
        };
        assert_eq!(poll.encode_to_vec(), vec![0x1a, 0x00]);
        let request = gnmi::SubscribeRequest {
            request: Some(gnmi::subscribe_request::Request::Subscribe(
                gnmi::SubscriptionList {
                    subscription: vec![gnmi::Subscription {
                        path: Some(path("/x")),
                        mode: 2,
                        sample_interval: 1_000_000_000,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        let decoded = gnmi::SubscribeRequest::decode(request.encode_to_vec().as_slice()).unwrap();
        let Some(gnmi::subscribe_request::Request::Subscribe(list)) = decoded.request else {
            panic!("not a subscribe request");
        };
        assert_eq!(list.subscription[0].sample_interval, 1_000_000_000);
    }

    #[test]
    fn reads_a_sync_response_from_subscribe_response() {
        let response = gnmi::SubscribeResponse {
            response: Some(gnmi::subscribe_response::Response::SyncResponse(true)),
            ..Default::default()
        };
        let decoded = gnmi::SubscribeResponse::decode(response.encode_to_vec().as_slice()).unwrap();
        assert_eq!(
            decoded.response,
            Some(gnmi::subscribe_response::Response::SyncResponse(true))
        );
    }

    #[test]
    fn unwraps_typed_values_the_way_the_ui_shows_them() {
        assert_eq!(
            decode_typed_value(Some(&typed(typed_value::Value::JsonIetfVal(
                br#"{"a":"1"}"#.to_vec()
            )))),
            value(GnmiValueType::JsonIetf, json!({"a": "1"}))
        );
        #[allow(deprecated)]
        let decimal = typed(typed_value::Value::DecimalVal(gnmi::Decimal64 {
            digits: -12345,
            precision: 2,
        }));
        assert_eq!(
            decode_typed_value(Some(&decimal)),
            value(GnmiValueType::Decimal, json!(-123.45))
        );
        let leaflist = typed(typed_value::Value::LeaflistVal(gnmi::ScalarArray {
            element: vec![
                typed(typed_value::Value::StringVal("a".into())),
                typed(typed_value::Value::IntVal(2)),
            ],
        }));
        assert_eq!(
            decode_typed_value(Some(&leaflist)),
            value(GnmiValueType::Leaflist, json!(["a", 2]))
        );
        assert_eq!(
            decode_typed_value(Some(&typed(typed_value::Value::BytesVal(b"hi".to_vec())))),
            value(GnmiValueType::Bytes, json!("aGk="))
        );
        assert_eq!(decode_typed_value(None), none());
        assert_eq!(
            int64_to_json(9_007_199_254_740_993_i64),
            json!("9007199254740993")
        );
        assert_eq!(decimal_to_json(5, 3), json!(0.005));
        assert_eq!(decimal_to_json(500, 2), json!(5));
    }

    #[test]
    fn joins_the_notification_prefix_into_every_update_and_delete_path() {
        let json = notification_to_json(&gnmi::Notification {
            timestamp: 1,
            prefix: Some(path("/interface[name=e1]")),
            update: vec![gnmi::Update {
                path: Some(path("/statistics/in-octets")),
                val: Some(typed(typed_value::Value::UintVal(5))),
                ..Default::default()
            }],
            delete: vec![path("/description")],
            ..Default::default()
        });
        assert_eq!(json.prefix.as_deref(), Some("/interface[name=e1]"));
        assert_eq!(
            json.updates[0].path,
            "/interface[name=e1]/statistics/in-octets"
        );
        assert_eq!(json.deletes, vec!["/interface[name=e1]/description"]);
    }

    #[test]
    fn keeps_the_order_of_a_path_elements_keys() {
        let text = "/acl/acl-filter[type=ipv4][name=cpm]";
        let decoded = gnmi::Path::decode(path(text).encode_to_vec().as_slice()).unwrap();
        assert_eq!(format_gnmi_path(&path_from_proto(Some(&decoded))), text);
    }

    #[test]
    fn picks_json_ietf_over_json_and_honours_the_hosts_choice() {
        use GnmiEncoding::*;
        assert_eq!(pick_encoding(&[Proto, Json, JsonIetf], None), JsonIetf);
        assert_eq!(pick_encoding(&[Proto, Json], None), Json);
        assert_eq!(pick_encoding(&[], None), Json);
        assert_eq!(pick_encoding(&[JsonIetf], Some(Proto)), Proto);
        assert_eq!(
            set_value_to_proto("\"x\"", GnmiSetEncoding::JsonIetf),
            typed(typed_value::Value::JsonIetfVal(b"\"x\"".to_vec()))
        );
        assert_eq!(
            set_value_to_proto("x", GnmiSetEncoding::Ascii),
            typed(typed_value::Value::AsciiVal("x".into()))
        );
    }
}
