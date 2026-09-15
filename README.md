# ProtoDuck

A DuckDB extension for deserializing and serializing Protocol Buffer messages stored in database columns.

Unlike file-based protobuf extensions, ProtoDuck operates on serialized protobuf data stored directly in table columns, making it ideal for working with protobuf-encoded data in data lakes and analytics pipelines.

## Features

- **Dynamic Schema Loading**: Load `.proto` schemas at runtime without recompilation
- **Full Type Support**: All protobuf types including scalar types, nested messages, repeated fields, maps, oneofs, and enums
- **Field Extraction**: Extract specific fields using dot-notation paths
- **JSON Conversion**: Convert protobuf messages to JSON and JSON back to serialized protobuf bytes

## Installation

### From the DuckDB Community Extensions repository (recommended)

Once ProtoDuck is published to the community extensions registry:

```sql
INSTALL protoduck FROM community;
LOAD protoduck;
```

### Building from source

Prerequisites:

- DuckDB v1.5.2
- Rust (stable, 1.80 or later)
- Python 3
- `make` and a C toolchain

```bash
git clone --recursive https://github.com/fcsnk/protoduck
cd protoduck
make configure
make release
make install
```

Useful development targets include `make debug`, `make test`, `make fmt`, `make lint`, `make check`, and `make clean`.

## Usage

### 1. Load your protobuf schema

```sql
SELECT proto_schema_add('
    syntax = "proto3";
    package myapp;

    message User {
        int32 id = 1;
        string name = 2;
        repeated string tags = 3;
    }
');
```

Or load a pre-compiled descriptor set:

```sql
SELECT proto_schema_add_binary(read_blob('schema.desc'));
```

### 2. Decode protobuf messages

```sql
SELECT proto_to_json(protobuf_column, 'myapp.User') AS user_data
FROM my_table;

-- proto_decode is an alias of proto_to_json
SELECT proto_decode(protobuf_column, 'myapp.User') AS user_data
FROM my_table;
```

### 3. Convert JSON to protobuf

`proto_from_json` converts JSON into the serialized protobuf wire format and returns a DuckDB `BLOB`. `json_to_proto` is an alias.

```sql
SELECT proto_from_json(
    '{"id":42,"name":"Alice","tags":["analytics","duckdb"]}',
    'myapp.User'
) AS protobuf_data;

-- Alias
SELECT json_to_proto('{"id":42,"name":"Alice"}', 'myapp.User');
```

The JSON representation follows the same conventions as `proto_to_json`: enum values can be names, bytes are base64 strings, repeated fields are arrays, and maps are JSON objects. Integer strings are also accepted for 64-bit integer values.

Because the result is the actual protobuf bytes, it can be passed directly to any DuckDB function expecting a `BLOB` containing a serialized protobuf message.

### 4. Extract specific fields

```sql
SELECT proto_get(data, 'myapp.User', 'name') AS user_name FROM my_table;
SELECT proto_get(data, 'myapp.User', 'tags[0]') AS first_tag FROM my_table;
```

### 5. Inspect a registered schema

```sql
SELECT proto_describe('myapp.User');
```

## Functions reference

| Function | Description |
|----------|-------------|
| `proto_schema_add(proto_content VARCHAR)` | Load schema from `.proto` content |
| `proto_schema_add_binary(descriptor_set BLOB)` | Load a compiled `FileDescriptorSet` |
| `proto_describe(message_type VARCHAR)` | Describe a message type |
| `proto_decode(data BLOB, message_type VARCHAR)` | Decode protobuf to JSON string |
| `proto_to_json(data BLOB, message_type VARCHAR)` | Decode protobuf to JSON string (alias of `proto_decode`) |
| `proto_from_json(json VARCHAR, message_type VARCHAR)` | Convert JSON to serialized protobuf `BLOB` |
| `json_to_proto(json VARCHAR, message_type VARCHAR)` | Alias of `proto_from_json` |
| `proto_get(data BLOB, message_type VARCHAR, field_path VARCHAR)` | Extract a specific field value |

## Field path syntax

`proto_get` accepts simple fields, nested fields, repeated-field indexes, map access, and combinations such as `orders[0].items[1].product.name`.

## Type mapping

For protobuf → JSON:

| Protobuf type | JSON representation |
|---|---|
| int32/int64/uint32/uint64 and fixed/signed variants | JSON number |
| float/double | JSON number |
| bool | JSON boolean |
| string | JSON string |
| bytes | Base64-encoded JSON string |
| enum | Enum name, or number if unknown |
| message | JSON object |
| repeated | JSON array |
| map | JSON object |

For JSON → protobuf, the reverse conversion is applied according to the registered protobuf descriptor. Unknown fields and incompatible values are rejected rather than silently discarded.

## License

MIT

## Contributing

Contributions are welcome — please open issues or pull requests on GitHub.
