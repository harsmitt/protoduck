//! ProtoDuck - DuckDB Extension for Protobuf Deserialization

mod descriptor_pool;
mod error;
mod json_to_proto;
mod type_mapping;
mod well_known_types;

use std::sync::Arc;

use duckdb::arrow::array::{Array, BinaryArray, RecordBatch, StringArray};
use duckdb::arrow::datatypes::DataType;
use duckdb::vscalar::arrow::{ArrowFunctionSignature, VArrowScalar};
use duckdb::{duckdb_entrypoint_c_api, Connection};

use crate::descriptor_pool::{
    add_schema_from_binary, add_schema_from_proto, decode_message, describe_message_type,
    DescriptorPoolState,
};
use crate::json_to_proto::json_to_proto;
use crate::type_mapping::{extract_field_value, message_to_json};
use crate::well_known_types::{json_to_proto_with_wkt, normalize_output_value};

struct ProtoSchemaAdd;
impl VArrowScalar for ProtoSchemaAdd {
    type State = DescriptorPoolState;
    fn invoke(state: &Self::State, input: RecordBatch) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let content_col = input.column(0).as_any().downcast_ref::<StringArray>().ok_or("Expected string array for proto content")?;
        let results: Vec<Option<String>> = content_col.iter().map(|content| -> Result<Option<String>, crate::error::ProtoDuckError> {
            match content { Some(v) => add_schema_from_proto(state, v).map(|types| Some(format!("Loaded {} message type(s): {}", types.len(), types.join(", ")))), None => Ok(None) }
        }).collect::<Result<Vec<_>, _>>()?;
        Ok(Arc::new(StringArray::from(results)))
    }
    fn signatures() -> Vec<ArrowFunctionSignature> { vec![ArrowFunctionSignature::exact(vec![DataType::Utf8], DataType::Utf8)] }
}

struct ProtoSchemaAddBinary;
impl VArrowScalar for ProtoSchemaAddBinary {
    type State = DescriptorPoolState;
    fn invoke(state: &Self::State, input: RecordBatch) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let blob_col = input.column(0).as_any().downcast_ref::<BinaryArray>().ok_or("Expected binary array for descriptor set")?;
        let results: Vec<Option<String>> = blob_col.iter().map(|blob| -> Result<Option<String>, crate::error::ProtoDuckError> {
            match blob { Some(v) => add_schema_from_binary(state, v).map(|types| Some(format!("Loaded {} message type(s): {}", types.len(), types.join(", ")))), None => Ok(None) }
        }).collect::<Result<Vec<_>, _>>()?;
        Ok(Arc::new(StringArray::from(results)))
    }
    fn signatures() -> Vec<ArrowFunctionSignature> { vec![ArrowFunctionSignature::exact(vec![DataType::Binary], DataType::Utf8)] }
}

struct ProtoDescribe;
impl VArrowScalar for ProtoDescribe {
    type State = DescriptorPoolState;
    fn invoke(state: &Self::State, input: RecordBatch) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let type_col = input.column(0).as_any().downcast_ref::<StringArray>().ok_or("Expected string array for message type")?;
        let results: Vec<Option<String>> = type_col.iter().map(|v| -> Result<Option<String>, crate::error::ProtoDuckError> { match v { Some(v) => describe_message_type(state, v).map(Some), None => Ok(None) } }).collect::<Result<Vec<_>, _>>()?;
        Ok(Arc::new(StringArray::from(results)))
    }
    fn signatures() -> Vec<ArrowFunctionSignature> { vec![ArrowFunctionSignature::exact(vec![DataType::Utf8], DataType::Utf8)] }
}

struct ProtoToJson;
impl VArrowScalar for ProtoToJson {
    type State = DescriptorPoolState;
    fn invoke(state: &Self::State, input: RecordBatch) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let blob_col = input.column(0).as_any().downcast_ref::<BinaryArray>().ok_or("Expected binary array for protobuf data")?;
        let type_col = input.column(1).as_any().downcast_ref::<StringArray>().ok_or("Expected string array for message type")?;
        let results: Vec<Option<String>> = blob_col.iter().zip(type_col.iter()).map(|(blob, mt)| -> Result<Option<String>, crate::error::ProtoDuckError> {
            match (blob, mt) {
                (Some(data), Some(mt)) => {
                    let msg = decode_message(state, data, mt)?;
                    let json = normalize_output_value(message_to_json(&msg)?, &msg)?;
                    Ok(Some(serde_json::to_string(&json).map_err(crate::error::ProtoDuckError::from)?))
                }
                _ => Ok(None),
            }
        }).collect::<Result<Vec<_>, _>>()?;
        Ok(Arc::new(StringArray::from(results)))
    }
    fn signatures() -> Vec<ArrowFunctionSignature> { vec![ArrowFunctionSignature::exact(vec![DataType::Binary, DataType::Utf8], DataType::Utf8)] }
}

struct JsonToProto;
impl VArrowScalar for JsonToProto {
    type State = DescriptorPoolState;
    fn invoke(state: &Self::State, input: RecordBatch) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let json_col = input.column(0).as_any().downcast_ref::<StringArray>().ok_or("Expected string array for JSON")?;
        let type_col = input.column(1).as_any().downcast_ref::<StringArray>().ok_or("Expected string array for message type")?;
        let results: Vec<Option<Vec<u8>>> = json_col.iter().zip(type_col.iter()).map(|(json, mt)| -> Result<Option<Vec<u8>>, crate::error::ProtoDuckError> {
            match (json, mt) { (Some(json), Some(mt)) => json_to_proto_with_wkt(state, json, mt).map(Some), _ => Ok(None) }
        }).collect::<Result<Vec<_>, _>>()?;
        Ok(Arc::new(BinaryArray::from(results)))
    }
    fn signatures() -> Vec<ArrowFunctionSignature> { vec![ArrowFunctionSignature::exact(vec![DataType::Utf8, DataType::Utf8], DataType::Binary)] }
}

struct ProtoGet;
impl VArrowScalar for ProtoGet {
    type State = DescriptorPoolState;
    fn invoke(state: &Self::State, input: RecordBatch) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let blob_col = input.column(0).as_any().downcast_ref::<BinaryArray>().ok_or("Expected binary array for protobuf data")?;
        let type_col = input.column(1).as_any().downcast_ref::<StringArray>().ok_or("Expected string array for message type")?;
        let path_col = input.column(2).as_any().downcast_ref::<StringArray>().ok_or("Expected string array for field path")?;
        let results: Vec<Option<String>> = blob_col.iter().zip(type_col.iter()).zip(path_col.iter()).map(|((blob, mt), path)| -> Result<Option<String>, crate::error::ProtoDuckError> {
            match (blob, mt, path) { (Some(data), Some(mt), Some(path)) => decode_message(state, data, mt).and_then(|msg| extract_field_value(&msg, path)).map(Some), _ => Ok(None) }
        }).collect::<Result<Vec<_>, _>>()?;
        Ok(Arc::new(StringArray::from(results)))
    }
    fn signatures() -> Vec<ArrowFunctionSignature> { vec![ArrowFunctionSignature::exact(vec![DataType::Binary, DataType::Utf8, DataType::Utf8], DataType::Utf8)] }
}

#[duckdb_entrypoint_c_api(ext_name = "protoduck", min_duckdb_version = "v1.5.2")]
pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn std::error::Error>> {
    let descriptor_state = DescriptorPoolState::default();
    con.register_scalar_function_with_state::<ProtoSchemaAdd>("proto_schema_add", &descriptor_state)?;
    con.register_scalar_function_with_state::<ProtoSchemaAddBinary>("proto_schema_add_binary", &descriptor_state)?;
    con.register_scalar_function_with_state::<ProtoDescribe>("proto_describe", &descriptor_state)?;
    con.register_scalar_function_with_state::<ProtoToJson>("proto_to_json", &descriptor_state)?;
    con.register_scalar_function_with_state::<ProtoToJson>("proto_decode", &descriptor_state)?;
    con.register_scalar_function_with_state::<JsonToProto>("proto_from_json", &descriptor_state)?;
    con.register_scalar_function_with_state::<JsonToProto>("json_to_proto", &descriptor_state)?;
    con.register_scalar_function_with_state::<ProtoGet>("proto_get", &descriptor_state)?;
    Ok(())
}
