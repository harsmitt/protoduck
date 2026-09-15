//! Type mapping between Protobuf and DuckDB types.

use chrono::{DateTime, SecondsFormat, Utc};
use prost_reflect::{DynamicMessage, FieldDescriptor, Kind, MapKey, ReflectMessage, Value};
use serde_json::Value as JsonValue;

use crate::error::{ProtoDuckError, Result};

pub fn message_to_json(message: &DynamicMessage) -> Result<JsonValue> {
    let mut obj = serde_json::Map::new();
    let descriptor = message.descriptor();
    for field in descriptor.fields() {
        let field_name = field.name().to_string();
        if message.has_field(&field) {
            let value = message.get_field(&field);
            obj.insert(field_name, value_to_json(&value, &field)?);
        } else if field.is_list() {
            obj.insert(field_name, JsonValue::Array(vec![]));
        } else if field.is_map() {
            obj.insert(field_name, JsonValue::Object(serde_json::Map::new()));
        }
    }
    Ok(JsonValue::Object(obj))
}

fn value_to_json(value: &Value, field: &FieldDescriptor) -> Result<JsonValue> {
    if is_timestamp_field(field) {
        if let Value::Message(msg) = value {
            return timestamp_to_json(msg);
        }
    }
    match value {
        Value::Bool(v) => Ok(JsonValue::Bool(*v)),
        Value::I32(v) => Ok(JsonValue::Number((*v).into())),
        Value::I64(v) => Ok(JsonValue::Number((*v).into())),
        Value::U32(v) => Ok(JsonValue::Number((*v).into())),
        Value::U64(v) => Ok(JsonValue::Number((*v).into())),
        Value::F32(v) => Ok(JsonValue::Number(
            serde_json::Number::from_f64(*v as f64)
                .unwrap_or_else(|| serde_json::Number::from(0)),
        )),
        Value::F64(v) => Ok(JsonValue::Number(
            serde_json::Number::from_f64(*v).unwrap_or_else(|| serde_json::Number::from(0)),
        )),
        Value::String(v) => Ok(JsonValue::String(v.clone())),
        Value::Bytes(v) => {
            use base64::Engine;
            Ok(JsonValue::String(
                base64::engine::general_purpose::STANDARD.encode(v),
            ))
        }
        Value::EnumNumber(n) => {
            if let Kind::Enum(desc) = field.kind() {
                if let Some(ev) = desc.get_value(*n) {
                    return Ok(JsonValue::String(ev.name().to_string()));
                }
            }
            Ok(JsonValue::Number((*n).into()))
        }
        Value::Message(msg) => message_to_json(msg),
        Value::List(list) => Ok(JsonValue::Array(
            list.iter()
                .map(|v| value_to_json(v, field))
                .collect::<Result<Vec<_>>>()?,
        )),
        Value::Map(map) => {
            let mut obj = serde_json::Map::new();
            for (key, val) in map {
                obj.insert(map_key_to_string(key), value_to_json(val, field)?);
            }
            Ok(JsonValue::Object(obj))
        }
    }
}

fn is_timestamp_field(field: &FieldDescriptor) -> bool {
    matches!(field.kind(), Kind::Message(ref msg) if msg.full_name() == "google.protobuf.Timestamp")
}

fn timestamp_to_json(message: &DynamicMessage) -> Result<JsonValue> {
    let descriptor = message.descriptor();
    let seconds_field = descriptor.get_field_by_name("seconds").ok_or_else(|| {
        ProtoDuckError::InvalidFieldValue {
            field: "seconds".to_string(),
            expected: "Timestamp seconds field".to_string(),
            actual: "missing".to_string(),
        }
    })?;
    let nanos_field = descriptor
        .get_field_by_name("nanos")
        .ok_or_else(|| ProtoDuckError::InvalidFieldValue {
            field: "nanos".to_string(),
            expected: "Timestamp nanos field".to_string(),
            actual: "missing".to_string(),
        })?;
    let seconds = match message.get_field(&seconds_field).into_owned() {
        Value::I64(v) => v,
        _ => {
            return Err(ProtoDuckError::InvalidFieldValue {
                field: "seconds".to_string(),
                expected: "int64".to_string(),
                actual: "invalid descriptor".to_string(),
            })
        }
    };
    let nanos = match message.get_field(&nanos_field).into_owned() {
        Value::I32(v) => v,
        _ => {
            return Err(ProtoDuckError::InvalidFieldValue {
                field: "nanos".to_string(),
                expected: "int32".to_string(),
                actual: "invalid descriptor".to_string(),
            })
        }
    };
    let timestamp = DateTime::<Utc>::from_timestamp(seconds, nanos as u32).ok_or_else(|| {
        ProtoDuckError::InvalidFieldValue {
            field: "timestamp".to_string(),
            expected: "valid protobuf Timestamp".to_string(),
            actual: format!("seconds={}, nanos={}", seconds, nanos),
        }
    })?;
    Ok(JsonValue::String(
        timestamp.to_rfc3339_opts(SecondsFormat::AutoSi, true),
    ))
}

fn map_key_to_string(key: &MapKey) -> String {
    match key {
        MapKey::Bool(v) => v.to_string(),
        MapKey::I32(v) => v.to_string(),
        MapKey::I64(v) => v.to_string(),
        MapKey::U32(v) => v.to_string(),
        MapKey::U64(v) => v.to_string(),
        MapKey::String(v) => v.clone(),
    }
}

pub fn extract_field_value(message: &DynamicMessage, path: &str) -> Result<String> {
    value_to_string(&navigate_to_value(message, path)?)
}

fn navigate_to_value(message: &DynamicMessage, path: &str) -> Result<Value> {
    let mut current = PathValue::new(Value::Message(message.clone()));
    for part in parse_field_path(path)? {
        current = apply_path_part(current, &part, path)?;
    }
    Ok(current.value)
}

struct PathValue {
    value: Value,
    map_key_field: Option<FieldDescriptor>,
}
impl PathValue {
    fn new(value: Value) -> Self {
        Self {
            value,
            map_key_field: None,
        }
    }
    fn with_map_key(value: Value, map_key_field: Option<FieldDescriptor>) -> Self {
        Self {
            value,
            map_key_field,
        }
    }
}

#[derive(Debug)]
enum PathPart {
    Field(String),
    Index(usize),
    MapKey(String),
}

fn parse_field_path(path: &str) -> Result<Vec<PathPart>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !current.is_empty() {
                    parts.push(PathPart::Field(current.clone()));
                    current.clear();
                }
            }
            '[' => {
                if !current.is_empty() {
                    parts.push(PathPart::Field(current.clone()));
                    current.clear();
                }
                let mut key = String::new();
                if let Some(&quote) = chars.peek() {
                    if quote == '\'' || quote == '"' {
                        chars.next();
                        for ch in chars.by_ref() {
                            if ch == quote {
                                break;
                            }
                            key.push(ch);
                        }
                        if chars.next() != Some(']') {
                            return Err(ProtoDuckError::InvalidFieldPath(
                                "Expected ']' after quoted map key".to_string(),
                            ));
                        }
                        parts.push(PathPart::MapKey(key));
                        continue;
                    }
                }
                for ch in chars.by_ref() {
                    if ch == ']' {
                        break;
                    }
                    key.push(ch);
                }
                if let Ok(idx) = key.parse::<usize>() {
                    parts.push(PathPart::Index(idx));
                } else {
                    parts.push(PathPart::MapKey(key));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(PathPart::Field(current));
    }
    if parts.is_empty() {
        return Err(ProtoDuckError::InvalidFieldPath(
            "Empty field path".to_string(),
        ));
    }
    Ok(parts)
}

fn apply_path_part(value: PathValue, part: &PathPart, original_path: &str) -> Result<PathValue> {
    let PathValue {
        value,
        map_key_field,
    } = value;
    match (value, part) {
        (Value::Message(msg), PathPart::Field(name)) => {
            let descriptor = msg.descriptor();
            let field = descriptor.get_field_by_name(name).ok_or_else(|| {
                ProtoDuckError::FieldNotFound(name.clone(), descriptor.full_name().to_string())
            })?;
            let value = msg.get_field(&field).into_owned();
            Ok(PathValue::with_map_key(
                resolve_enum_names(value, &field),
                map_key_field_descriptor(&field),
            ))
        }
        (Value::List(list), PathPart::Index(idx)) => {
            if *idx >= list.len() {
                return Err(ProtoDuckError::IndexOutOfBounds(
                    *idx,
                    original_path.to_string(),
                    list.len(),
                ));
            }
            Ok(PathValue::new(list[*idx].clone()))
        }
        (Value::Map(map), PathPart::MapKey(key)) => {
            let map_key = parse_map_key(key, map_key_field.as_ref(), original_path)?;
            map.get(&map_key)
                .cloned()
                .map(PathValue::new)
                .ok_or_else(|| {
                    ProtoDuckError::MapKeyNotFound(key.clone(), original_path.to_string())
                })
        }
        (Value::Map(map), PathPart::Index(idx)) => {
            let key = idx.to_string();
            let map_key = parse_map_key(&key, map_key_field.as_ref(), original_path)?;
            map.get(&map_key)
                .cloned()
                .map(PathValue::new)
                .ok_or_else(|| ProtoDuckError::MapKeyNotFound(key, original_path.to_string()))
        }
        (Value::List(_), PathPart::Field(f)) => Err(ProtoDuckError::InvalidFieldPath(format!(
            "Cannot access field '{}' on a repeated value - use an index first",
            f
        ))),
        (_, PathPart::Index(_)) => {
            Err(ProtoDuckError::NotARepeatedField(original_path.to_string()))
        }
        (_, PathPart::MapKey(k)) => Err(ProtoDuckError::InvalidFieldPath(format!(
            "Cannot access map key '{}' on non-map value",
            k
        ))),
        (_, PathPart::Field(f)) => Err(ProtoDuckError::InvalidFieldPath(format!(
            "Cannot access field '{}' on non-message value",
            f
        ))),
    }
}

fn map_key_field_descriptor(field: &FieldDescriptor) -> Option<FieldDescriptor> {
    if let Kind::Message(message) = field.kind() {
        if field.is_map() {
            return Some(message.map_entry_key_field());
        }
    }
    None
}

fn parse_map_key(
    key: &str,
    key_field: Option<&FieldDescriptor>,
    original_path: &str,
) -> Result<MapKey> {
    let Some(field) = key_field else {
        return Err(ProtoDuckError::InvalidFieldPath(format!(
            "Cannot resolve map key '{}' without map key type metadata in '{}'",
            key, original_path
        )));
    };
    match field.kind() {
        Kind::Bool => key
            .parse::<bool>()
            .map(MapKey::Bool)
            .map_err(|_| invalid_map_key(key, field, original_path)),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => key
            .parse::<i32>()
            .map(MapKey::I32)
            .map_err(|_| invalid_map_key(key, field, original_path)),
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => key
            .parse::<i64>()
            .map(MapKey::I64)
            .map_err(|_| invalid_map_key(key, field, original_path)),
        Kind::Uint32 | Kind::Fixed32 => key
            .parse::<u32>()
            .map(MapKey::U32)
            .map_err(|_| invalid_map_key(key, field, original_path)),
        Kind::Uint64 | Kind::Fixed64 => key
            .parse::<u64>()
            .map(MapKey::U64)
            .map_err(|_| invalid_map_key(key, field, original_path)),
        Kind::String => Ok(MapKey::String(key.to_string())),
        _ => Err(ProtoDuckError::InvalidFieldPath(format!(
            "Unsupported map key type for '{}' in '{}'",
            key, original_path
        ))),
    }
}

fn invalid_map_key(key: &str, field: &FieldDescriptor, path: &str) -> ProtoDuckError {
    ProtoDuckError::InvalidFieldPath(format!(
        "Map key '{}' cannot be parsed as {} in '{}'",
        key,
        field_kind_name(field),
        path
    ))
}

fn field_kind_name(field: &FieldDescriptor) -> &'static str {
    match field.kind() {
        Kind::Double => "double",
        Kind::Float => "float",
        Kind::Int64 => "int64",
        Kind::Uint64 => "uint64",
        Kind::Int32 => "int32",
        Kind::Fixed64 => "fixed64",
        Kind::Fixed32 => "fixed32",
        Kind::Bool => "bool",
        Kind::String => "string",
        Kind::Bytes => "bytes",
        Kind::Uint32 => "uint32",
        Kind::Sfixed32 => "sfixed32",
        Kind::Sfixed64 => "sfixed64",
        Kind::Sint32 => "sint32",
        Kind::Sint64 => "sint64",
        Kind::Enum(_) => "enum",
        Kind::Message(_) => "message",
    }
}

fn resolve_enum_names(value: Value, field: &FieldDescriptor) -> Value {
    match value {
        Value::EnumNumber(n) => {
            if let Kind::Enum(desc) = field.kind() {
                if let Some(ev) = desc.get_value(n) {
                    return Value::String(ev.name().to_string());
                }
            }
            Value::EnumNumber(n)
        }
        Value::List(list) => Value::List(
            list.into_iter()
                .map(|v| resolve_enum_names(v, field))
                .collect(),
        ),
        Value::Map(map) => Value::Map(
            map.into_iter()
                .map(|(k, v)| (k, resolve_enum_names(v, field)))
                .collect(),
        ),
        other => other,
    }
}

fn value_to_string(value: &Value) -> Result<String> {
    match value {
        Value::Bool(v) => Ok(v.to_string()),
        Value::I32(v) => Ok(v.to_string()),
        Value::I64(v) => Ok(v.to_string()),
        Value::U32(v) => Ok(v.to_string()),
        Value::U64(v) => Ok(v.to_string()),
        Value::F32(v) => Ok(v.to_string()),
        Value::F64(v) => Ok(v.to_string()),
        Value::String(v) => Ok(v.clone()),
        Value::Bytes(v) => {
            use base64::Engine;
            Ok(base64::engine::general_purpose::STANDARD.encode(v))
        }
        Value::EnumNumber(v) => Ok(v.to_string()),
        Value::Message(msg) => Ok(serde_json::to_string(&message_to_json(msg)?)?),
        Value::List(list) => Ok(format!(
            "[{}]",
            list.iter()
                .map(value_to_string)
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        )),
        Value::Map(map) => Ok(format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| Ok(format!("{}: {}", map_key_to_string(k), value_to_string(v)?)))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor_pool::{
        add_schema_from_proto, get_message_descriptor, DescriptorPoolState,
    };
    use std::collections::HashMap;

    #[test]
    fn test_parse_simple_path() {
        let parts = parse_field_path("name").unwrap();
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], PathPart::Field(s) if s == "name"));
    }

    #[test]
    fn test_parse_nested_path() {
        assert_eq!(parse_field_path("user.address.street").unwrap().len(), 3);
    }

    #[test]
    fn test_parse_array_index() {
        let parts = parse_field_path("items[0]").unwrap();
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[1], PathPart::Index(0)));
    }

    #[test]
    fn test_parse_map_key() {
        let parts = parse_field_path("properties['key']").unwrap();
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[1], PathPart::MapKey(s) if s == "key"));
    }

    #[test]
    fn test_extract_map_key_uses_declared_key_type() {
        let message = map_test_message();
        assert_eq!(
            extract_field_value(&message, "int64_values[1]").unwrap(),
            "signed"
        );
        assert_eq!(
            extract_field_value(&message, "uint64_values[1]").unwrap(),
            "unsigned"
        );
        assert_eq!(
            extract_field_value(&message, "string_values[\"1\"]").unwrap(),
            "string"
        );
    }

    #[test]
    fn test_extract_map_key_rejects_invalid_declared_type() {
        let message = map_test_message();
        let err = extract_field_value(&message, "uint64_values[-1]").unwrap_err();
        assert!(matches!(err, ProtoDuckError::InvalidFieldPath(_)));
    }

    fn map_test_message() -> DynamicMessage {
        let proto = r#"syntax = "proto3"; package maptest; message Maps { map<int64, string> int64_values = 1; map<uint64, string> uint64_values = 2; map<string, string> string_values = 3; }"#;
        let state = DescriptorPoolState::default();
        add_schema_from_proto(&state, proto).unwrap();
        let descriptor = get_message_descriptor(&state, "maptest.Maps").unwrap();
        let mut message = DynamicMessage::new(descriptor);
        message.set_field_by_name(
            "int64_values",
            Value::Map(HashMap::from([(
                MapKey::I64(1),
                Value::String("signed".to_string()),
            )])),
        );
        message.set_field_by_name(
            "uint64_values",
            Value::Map(HashMap::from([(
                MapKey::U64(1),
                Value::String("unsigned".to_string()),
            )])),
        );
        message.set_field_by_name(
            "string_values",
            Value::Map(HashMap::from([(
                MapKey::String("1".to_string()),
                Value::String("string".to_string()),
            )])),
        );
        message
    }
}
