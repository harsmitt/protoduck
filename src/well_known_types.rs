//! Protobuf well-known type JSON normalization.
//!
//! ProtoDuck's general JSON mapping predates ProtoJSON's special well-known
//! type encodings. Timestamp is represented by seconds/nanos internally but
//! must be represented as an RFC 3339 string in canonical JSON.

use prost_reflect::{DynamicMessage, FieldDescriptor, Kind, MessageDescriptor};
use serde::de::DeserializeSeed;
use serde_json::Value as JsonValue;

use crate::descriptor_pool::{get_message_descriptor, DescriptorPoolState};
use crate::error::{ProtoDuckError, Result};
use crate::json_to_proto::json_to_proto;

const TIMESTAMP: &str = "google.protobuf.Timestamp";

/// Convert canonical ProtoJSON Timestamp strings to the neutral object form
/// consumed by the existing JSON -> protobuf converter.
pub fn json_to_proto_with_wkt(
    state: &DescriptorPoolState,
    json: &str,
    message_type: &str,
) -> Result<Vec<u8>> {
    let value: JsonValue = serde_json::from_str(json).map_err(|err| {
        ProtoDuckError::JsonDeserializeError(format!("Invalid JSON: {}", err))
    })?;
    let descriptor = get_message_descriptor(state, message_type)?;
    let normalized = normalize_input_value(&value, &descriptor)?;
    let normalized_json = serde_json::to_string(&normalized)
        .map_err(|err| ProtoDuckError::JsonSerializeError(err.to_string()))?;
    json_to_proto(state, &normalized_json, message_type)
}

/// Convert the neutral object representation emitted by ProtoDuck into
/// canonical ProtoJSON for well-known Timestamp fields.
pub fn normalize_output_value(
    value: JsonValue,
    message: &DynamicMessage,
) -> Result<JsonValue> {
    normalize_output_message(value, message.descriptor())
}

fn normalize_input_value(value: &JsonValue, descriptor: &MessageDescriptor) -> Result<JsonValue> {
    let object = value.as_object().ok_or_else(|| ProtoDuckError::InvalidFieldValue {
        field: "<message>".to_string(),
        expected: format!("JSON object for {}", descriptor.full_name()),
        actual: json_type(value).to_string(),
    })?;

    let mut result = serde_json::Map::new();
    for (name, value) in object {
        let field = descriptor.get_field_by_name(name).ok_or_else(|| {
            ProtoDuckError::FieldNotFound(name.clone(), descriptor.full_name().to_string())
        })?;
        result.insert(name.clone(), normalize_input_field(value, &field)?);
    }
    Ok(JsonValue::Object(result))
}

fn normalize_input_field(value: &JsonValue, field: &FieldDescriptor) -> Result<JsonValue> {
    if field.is_list() {
        let values = value.as_array().ok_or_else(|| invalid_value(field, "array", value))?;
        return Ok(JsonValue::Array(
            values
                .iter()
                .map(|v| normalize_input_scalar(v, field.kind()))
                .collect::<Result<Vec<_>>>()?,
        ));
    }

    if field.is_map() {
        let object = value.as_object().ok_or_else(|| invalid_value(field, "object", value))?;
        let entry = match field.kind() {
            Kind::Message(entry) => entry,
            _ => unreachable!(),
        };
        let value_field = entry.map_entry_value_field();
        let mut result = serde_json::Map::new();
        for (key, value) in object {
            result.insert(
                key.clone(),
                normalize_input_scalar(value, value_field.kind())?,
            );
        }
        return Ok(JsonValue::Object(result));
    }

    normalize_input_scalar(value, field.kind())
}

fn normalize_input_scalar(value: &JsonValue, kind: Kind) -> Result<JsonValue> {
    match kind {
        Kind::Message(descriptor) if descriptor.full_name() == TIMESTAMP => {
            if !value.is_string() {
                return Ok(value.clone());
            }
            let text = value.to_string();
            let mut deserializer = serde_json::Deserializer::from_str(&text);
            let timestamp = descriptor.deserialize(&mut deserializer).map_err(|err| {
                ProtoDuckError::JsonDeserializeError(format!("Invalid Timestamp: {}", err))
            })?;
            deserializer.end().map_err(|err| {
                ProtoDuckError::JsonDeserializeError(format!("Invalid Timestamp: {}", err))
            })?;
            // Existing json_to_proto understands the neutral {seconds,nanos}
            // representation, so use ProtoDuck's existing mapper after the
            // canonical Timestamp parser has validated the RFC 3339 string.
            let mut object = serde_json::Map::new();
            for field in timestamp.descriptor().fields() {
                let json = match timestamp.get_field(&field).as_ref() {
                    prost_reflect::Value::I64(v) => JsonValue::Number((*v).into()),
                    prost_reflect::Value::I32(v) => JsonValue::Number((*v).into()),
                    _ => continue,
                };
                object.insert(field.name().to_string(), json);
            }
            Ok(JsonValue::Object(object))
        }
        Kind::Message(descriptor) => normalize_input_value(value, &descriptor),
        _ => Ok(value.clone()),
    }
}

fn normalize_output_message(value: JsonValue, descriptor: MessageDescriptor) -> Result<JsonValue> {
    if descriptor.full_name() == TIMESTAMP {
        let text = serde_json::to_string(&value)
            .map_err(|err| ProtoDuckError::JsonSerializeError(err.to_string()))?;
        let mut deserializer = serde_json::Deserializer::from_str(&text);
        let timestamp = descriptor.deserialize(&mut deserializer).map_err(|err| {
            ProtoDuckError::JsonSerializeError(format!("Invalid Timestamp: {}", err))
        })?;
        deserializer.end().map_err(|err| {
            ProtoDuckError::JsonSerializeError(format!("Invalid Timestamp: {}", err))
        })?;
        return serde_json::to_value(&timestamp)
            .map_err(|err| ProtoDuckError::JsonSerializeError(err.to_string()));
    }

    let Some(object) = value.as_object() else { return Ok(value); };
    let mut result = serde_json::Map::new();

    for (name, value) in object {
        let Some(field) = descriptor.get_field_by_name(name) else {
            result.insert(name.clone(), value.clone());
            continue;
        };
        result.insert(name.clone(), normalize_output_field(value.clone(), &field)?);
    }
    Ok(JsonValue::Object(result))
}

fn normalize_output_field(value: JsonValue, field: &FieldDescriptor) -> Result<JsonValue> {
    if field.is_list() {
        let Some(values) = value.as_array() else { return Ok(value); };
        return Ok(JsonValue::Array(
            values
                .iter()
                .map(|v| normalize_output_scalar(v.clone(), field.kind()))
                .collect::<Result<Vec<_>>>()?,
        ));
    }

    if field.is_map() {
        let Some(object) = value.as_object() else { return Ok(value); };
        let entry = match field.kind() {
            Kind::Message(entry) => entry,
            _ => unreachable!(),
        };
        let value_kind = entry.map_entry_value_field().kind();
        let mut result = serde_json::Map::new();
        for (key, value) in object {
            result.insert(key.clone(), normalize_output_scalar(value.clone(), value_kind)?);
        }
        return Ok(JsonValue::Object(result));
    }

    normalize_output_scalar(value, field.kind())
}

fn normalize_output_scalar(value: JsonValue, kind: Kind) -> Result<JsonValue> {
    match kind {
        Kind::Message(descriptor) => normalize_output_message(value, descriptor),
        _ => Ok(value),
    }
}

fn invalid_value(field: &FieldDescriptor, expected: &str, value: &JsonValue) -> ProtoDuckError {
    ProtoDuckError::InvalidFieldValue {
        field: field.name().to_string(),
        expected: expected.to_string(),
        actual: json_type(value).to_string(),
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
