mod descriptor_pool;
mod error;
mod json_to_proto;
mod type_mapping;

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

// ============================================================================
// Schema registration
// ============================================================================

struct ProtoSchemaAdd;

impl VArrowScalar for ProtoSchemaAdd {
    type State = DescriptorPoolState;

    fn invoke(
        state: &Self::State,
        input: RecordBatch,
    ) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let col = input
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Expected string array")?;

        let results: Vec<Option<String>> = col
            .iter()
            .map(|value| match value {
                Some(proto) => {
                    add_schema_from_proto(state, proto).map(|names| Some(names.join(",")))
                }
                None => Ok(None),
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Arc::new(StringArray::from(results)))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![DataType::Utf8],
            DataType::Utf8,
        )]
    }
}

struct ProtoSchemaAddBinary;

impl VArrowScalar for ProtoSchemaAddBinary {
    type State = DescriptorPoolState;

    fn invoke(
        state: &Self::State,
        input: RecordBatch,
    ) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let col = input
            .column(0)
            .as_any()
            .downcast_ref::<BinaryArray>()
            .ok_or("Expected binary array")?;

        let results: Vec<Option<String>> = col
            .iter()
            .map(|value| match value {
                Some(bytes) => {
                    add_schema_from_binary(state, bytes).map(|names| Some(names.join(",")))
                }
                None => Ok(None),
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Arc::new(StringArray::from(results)))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![DataType::Binary],
            DataType::Utf8,
        )]
    }
}

// ============================================================================
// Schema description
// ============================================================================

struct ProtoDescribe;

impl VArrowScalar for ProtoDescribe {
    type State = DescriptorPoolState;

    fn invoke(
        state: &Self::State,
        input: RecordBatch,
    ) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let col = input
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Expected string array")?;

        let results: Vec<Option<String>> = col
            .iter()
            .map(|value| match value {
                Some(message_type) => describe_message_type(state, message_type).map(Some),
                None => Ok(None),
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Arc::new(StringArray::from(results)))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![DataType::Utf8],
            DataType::Utf8,
        )]
    }
}

// ============================================================================
// Protobuf -> JSON
// ============================================================================

struct ProtoToJson;

impl VArrowScalar for ProtoToJson {
    type State = DescriptorPoolState;

    fn invoke(
        state: &Self::State,
        input: RecordBatch,
    ) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let data_col = input
            .column(0)
            .as_any()
            .downcast_ref::<BinaryArray>()
            .ok_or("Expected binary array for protobuf data")?;
        let type_col = input
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Expected string array for message type")?;

        let results: Vec<Option<String>> = data_col
            .iter()
            .zip(type_col.iter())
            .map(
                |(data, message_type)| -> Result<Option<String>, crate::error::ProtoDuckError> {
                    match (data, message_type) {
                        (Some(data), Some(mt)) => {
                            let message = decode_message(state, data, mt)?;
                            Ok(Some(serde_json::to_string(&message_to_json(&message)?)?))
                        }
                        _ => Ok(None),
                    }
                },
            )
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Arc::new(StringArray::from(results)))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![DataType::Binary, DataType::Utf8],
            DataType::Utf8,
        )]
    }
}

// ============================================================================
// JSON -> Protobuf
// ============================================================================

struct ProtoFromJson;

impl VArrowScalar for ProtoFromJson {
    type State = DescriptorPoolState;

    fn invoke(
        state: &Self::State,
        input: RecordBatch,
    ) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let json_col = input
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Expected string array for JSON")?;
        let type_col = input
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Expected string array for message type")?;

        let results: Vec<Option<Vec<u8>>> = json_col
            .iter()
            .zip(type_col.iter())
            .map(
                |(json, message_type)| -> Result<Option<Vec<u8>>, crate::error::ProtoDuckError> {
                    match (json, message_type) {
                        (Some(json), Some(mt)) => json_to_proto(state, json, mt).map(Some),
                        _ => Ok(None),
                    }
                },
            )
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Arc::new(BinaryArray::from_iter(results)))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![DataType::Utf8, DataType::Utf8],
            DataType::Binary,
        )]
    }
}

// ============================================================================
// Proto Get Function
// ============================================================================

struct ProtoGet;

impl VArrowScalar for ProtoGet {
    type State = DescriptorPoolState;

    fn invoke(
        state: &Self::State,
        input: RecordBatch,
    ) -> Result<Arc<dyn Array>, Box<dyn std::error::Error>> {
        let data_col = input
            .column(0)
            .as_any()
            .downcast_ref::<BinaryArray>()
            .ok_or("Expected binary array for protobuf data")?;
        let type_col = input
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Expected string array for message type")?;
        let path_col = input
            .column(2)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("Expected string array for field path")?;

        let results: Vec<Option<String>> = data_col
            .iter()
            .zip(type_col.iter())
            .zip(path_col.iter())
            .map(|((data, message_type), path)| match (data, message_type, path) {
                (Some(data), Some(mt), Some(path)) => {
                    let message = decode_message(state, data, mt)?;
                    Ok(Some(extract_field_value(&message, path)?))
                }
                _ => Ok(None),
            })
            .collect::<Result<Vec<_>, crate::error::ProtoDuckError>>()?;

        Ok(Arc::new(StringArray::from(results)))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![DataType::Binary, DataType::Utf8, DataType::Utf8],
            DataType::Utf8,
        )]
    }
}

#[duckdb_entrypoint_c_api]
pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), duckdb::Error> {
    let descriptor_state: DescriptorPoolState = Arc::new(Default::default());

    con.register_scalar_function_with_state::<ProtoSchemaAdd>(
        "proto_schema_add",
        &descriptor_state,
    )?;
    con.register_scalar_function_with_state::<ProtoSchemaAddBinary>(
        "proto_schema_add_binary",
        &descriptor_state,
    )?;
    con.register_scalar_function_with_state::<ProtoDescribe>(
        "proto_describe",
        &descriptor_state,
    )?;
    con.register_scalar_function_with_state::<ProtoToJson>("proto_to_json", &descriptor_state)?;
    con.register_scalar_function_with_state::<ProtoToJson>("proto_decode", &descriptor_state)?;
    con.register_scalar_function_with_state::<ProtoFromJson>(
        "proto_from_json",
        &descriptor_state,
    )?;
    con.register_scalar_function_with_state::<ProtoFromJson>(
        "json_to_proto",
        &descriptor_state,
    )?;
    con.register_scalar_function_with_state::<ProtoGet>("proto_get", &descriptor_state)?;

    Ok(())
}
