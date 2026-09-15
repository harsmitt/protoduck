//! JSON to Protocol Buffer conversion using the runtime descriptor pool.

use std::collections::HashMap;

use base64::Engine;
use chrono::DateTime;
use prost::Message;
use prost_reflect::{DynamicMessage, FieldDescriptor, Kind, MapKey, Value};
use serde_json::Value as JsonValue;

use crate::descriptor_pool::{get_message_descriptor, DescriptorPoolState};
use crate::error::{ProtoDuckError, Result};

/// Convert JSON into a serialized protobuf message using a descriptor loaded in
/// the ProtoDuck descriptor pool.
pub fn json_to_proto(
    state: &DescriptorPoolState,
    json: &str,
    message_type: &str,
) -> Result<Vec<u8>> {
    let value: JsonValue = serde_json::from_str(json)
        .map_err(|err| ProtoDuckError::JsonDeserializeError(format!("Invalid JSON: {}", err)))?;

    let descriptor = get_message_descriptor(state, message_type)?;
    let message = json_value_to_message(&value, descriptor, message_type)?;
    Ok(message.encode_to_vec())
}

fn json_value_to_message(
    value: &JsonValue,
    descriptor: prost_reflect::MessageDescriptor,
    message_type: &str,
) -> Result<DynamicMessage> {
    let object = value
        .as_object()
        .ok_or_else(|| ProtoDuckError::InvalidFieldValue {
            field: "<message>".to_string(),
            expected: format!("JSON object for {}", message_type),
            actual: json_type(value).to_string(),
        })?;

    let mut message = DynamicMessage::new(descriptor.clone());
    for (field_name, json_value) in object {
        let field = descriptor.get_field_by_name(field_name).ok_or_else(|| {
            ProtoDuckError::FieldNotFound(field_name.clone(), descriptor.full_name().to_string())
        })?;
        let value = json_value_to_field_value(json_value, &field)?;
        message.set_field(&field, value);
    }
    Ok(message)
}

fn json_value_to_field_value(value: &JsonValue, field: &FieldDescriptor) -> Result<Value> {
    if field.is_list() {
        let values = value
            .as_array()
            .ok_or_else(|| invalid_value(field, "array", value))?;
        let items = values
            .iter()
            .map(|item| json_value_to_scalar_value(item, field, field.kind()))
            .collect::<Result<Vec<_>>>()?;
        return Ok(Value::List(items));
    }

    if field.is_map() {
        let object = value
            .as_object()
            .ok_or_else(|| invalid_value(field, "object", value))?;
        let entry = match field.kind() {
            Kind::Message(entry) => entry,
            _ => unreachable!("protobuf map fields always have a map-entry message kind"),
        };
        let key_field = entry.map_entry_key_field();
        let value_field = entry.map_entry_value_field();
        let mut map = HashMap::new();

        for (key, json_value) in object {
            let map_key = json_string_to_map_key(key, &key_field)?;
            let map_value =
                json_value_to_scalar_value(json_value, &value_field, value_field.kind())?;
            map.insert(map_key, map_value);
        }
        return Ok(Value::Map(map));
    }

    json_value_to_scalar_value(value, field, field.kind())
}

fn json_value_to_scalar_value(
    value: &JsonValue,
    field: &FieldDescriptor,
    kind: Kind,
) -> Result<Value> {
    match kind {
        Kind::Bool => value
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| invalid_value(field, "boolean", value)),
        Kind::String => value
            .as_str()
            .map(|v| Value::String(v.to_string()))
            .ok_or_else(|| invalid_value(field, "string", value)),
        Kind::Bytes => {
            let encoded = value
                .as_str()
                .ok_or_else(|| invalid_value(field, "base64 string", value))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|err| ProtoDuckError::InvalidFieldValue {
                    field: field.name().to_string(),
                    expected: "valid base64 string".to_string(),
                    actual: err.to_string(),
                })?;
            Ok(Value::Bytes(bytes.into()))
        }
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
            let n = i32::try_from(json_i64(value, field)?)
                .map_err(|_| invalid_value(field, "int32", value))?;
            Ok(Value::I32(n))
        }
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => Ok(Value::I64(json_i64(value, field)?)),
        Kind::Uint32 | Kind::Fixed32 => {
            let n = u32::try_from(json_u64(value, field)?)
                .map_err(|_| invalid_value(field, "uint32", value))?;
            Ok(Value::U32(n))
        }
        Kind::Uint64 | Kind::Fixed64 => Ok(Value::U64(json_u64(value, field)?)),
        Kind::Float => {
            let n = json_f64(value, field)? as f32;
            if !n.is_finite() {
                return Err(invalid_value(field, "finite float", value));
            }
            Ok(Value::F32(n))
        }
        Kind::Double => {
            let n = json_f64(value, field)?;
            if !n.is_finite() {
                return Err(invalid_value(field, "finite double", value));
            }
            Ok(Value::F64(n))
        }
        Kind::Enum(enum_descriptor) => match value {
            JsonValue::String(name) => enum_descriptor
                .get_value_by_name(name)
                .map(|enum_value| Value::EnumNumber(enum_value.number()))
                .ok_or_else(|| ProtoDuckError::InvalidFieldValue {
                    field: field.name().to_string(),
                    expected: format!("enum {} name", enum_descriptor.full_name()),
                    actual: name.clone(),
                }),
            JsonValue::Number(_) => {
                let n = i32::try_from(json_i64(value, field)?)
                    .map_err(|_| invalid_value(field, "enum number", value))?;
                Ok(Value::EnumNumber(n))
            }
            _ => Err(invalid_value(field, "enum name or number", value)),
        },
        Kind::Message(message_descriptor) => {
            if message_descriptor.full_name() == "google.protobuf.Timestamp" {
                return json_timestamp_to_value(value, field, message_descriptor);
            }

            let object = value
                .as_object()
                .ok_or_else(|| invalid_value(field, "object", value))?;
            let mut message = DynamicMessage::new(message_descriptor.clone());
            for (nested_name, nested_value) in object {
                let nested_field = message_descriptor
                    .get_field_by_name(nested_name)
                    .ok_or_else(|| {
                        ProtoDuckError::FieldNotFound(
                            nested_name.clone(),
                            message_descriptor.full_name().to_string(),
                        )
                    })?;
                let nested = json_value_to_field_value(nested_value, &nested_field)?;
                message.set_field(&nested_field, nested);
            }
            Ok(Value::Message(message))
        }
    }
}

fn json_timestamp_to_value(
    value: &JsonValue,
    field: &FieldDescriptor,
    descriptor: prost_reflect::MessageDescriptor,
) -> Result<Value> {
    let text = value
        .as_str()
        .ok_or_else(|| invalid_value(field, "RFC3339 timestamp string", value))?;
    let parsed = DateTime::parse_from_rfc3339(text).map_err(|_| invalid_value(
        field,
        "RFC3339 timestamp string",
        value,
    ))?;

    let seconds_field = descriptor
        .get_field_by_name("seconds")
        .ok_or_else(|| invalid_value(field, "valid google.protobuf.Timestamp", value))?;
    let nanos_field = descriptor
        .get_field_by_name("nanos")
        .ok_or_else(|| invalid_value(field, "valid google.protobuf.Timestamp", value))?;

    let mut message = DynamicMessage::new(descriptor);
    message.set_field(&seconds_field, Value::I64(parsed.timestamp()));
    message.set_field(
        &nanos_field,
        Value::I32(parsed.timestamp_subsec_nanos() as i32),
    );
    Ok(Value::Message(message))
}

fn json_string_to_map_key(key: &str, field: &FieldDescriptor) -> Result<MapKey> {
    match field.kind() {
        Kind::Bool => key
            .parse::<bool>()
            .map(MapKey::Bool)
            .map_err(|_| invalid_map_key(field, key)),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => key
            .parse::<i32>()
            .map(MapKey::I32)
            .map_err(|_| invalid_map_key(field, key)),
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => key
            .parse::<i64>()
            .map(MapKey::I64)
            .map_err(|_| invalid_map_key(field, key)),
        Kind::Uint32 | Kind::Fixed32 => key
            .parse::<u32>()
            .map(MapKey::U32)
            .map_err(|_| invalid_map_key(field, key)),
        Kind::Uint64 | Kind::Fixed64 => key
            .parse::<u64>()
            .map(MapKey::U64)
            .map_err(|_| invalid_map_key(field, key)),
        Kind::String => Ok(MapKey::String(key.to_string())),
        _ => Err(invalid_map_key(field, key)),
    }
}

fn json_i64(value: &JsonValue, field: &FieldDescriptor) -> Result<i64> {
    if let Some(n) = value.as_i64() {
        return Ok(n);
    }
    value
        .as_str()
        .ok_or_else(|| invalid_value(field, "integer or integer string", value))?
        .parse::<i64>()
        .map_err(|_| invalid_value(field, "integer or integer string", value))
}

fn json_u64(value: &JsonValue, field: &FieldDescriptor) -> Result<u64> {
    if let Some(n) = value.as_u64() {
        return Ok(n);
    }
    value
        .as_str()
        .ok_or_else(|| invalid_value(field, "unsigned integer or integer string", value))?
        .parse::<u64>()
        .map_err(|_| invalid_value(field, "unsigned integer or integer string", value))
}

fn json_f64(value: &JsonValue, field: &FieldDescriptor) -> Result<f64> {
    value
        .as_f64()
        .ok_or_else(|| invalid_value(field, "number", value))
}

fn invalid_value(field: &FieldDescriptor, expected: &str, value: &JsonValue) -> ProtoDuckError {
    ProtoDuckError::InvalidFieldValue {
        field: field.name().to_string(),
        expected: expected.to_string(),
        actual: json_type(value).to_string(),
    }
}

fn invalid_map_key(field: &FieldDescriptor, key: &str) -> ProtoDuckError {
    ProtoDuckError::InvalidFieldValue {
        field: field.name().to_string(),
        expected: "valid protobuf map key".to_string(),
        actual: key.to_string(),
    }
}

fn json_type(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}
