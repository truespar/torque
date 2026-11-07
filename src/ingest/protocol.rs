use crate::schema::{DocId, Document, FieldType, FieldValue, Language, Schema};

// -- Error --

#[derive(Debug)]
pub struct ProtocolError {
    pub message: String,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProtocolError {}

impl ProtocolError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

// -- Message type constants --

pub const MSG_START_INGEST: u8 = 0x01;
pub const MSG_DOC_BATCH: u8 = 0x02;
pub const MSG_COMMIT: u8 = 0x03;
pub const MSG_ABORT: u8 = 0x04;
pub const MSG_UPSERT: u8 = 0x05;

pub const MSG_ACK: u8 = 0x10;
pub const MSG_COMMIT_RESULT: u8 = 0x11;
pub const MSG_ERROR: u8 = 0x12;
pub const MSG_BACKPRESSURE: u8 = 0x13;

// -- Error codes --

pub const ERR_COLLECTION_NOT_FOUND: u16 = 1;
pub const ERR_SCHEMA_MISMATCH: u16 = 2;
pub const ERR_SESSION_ALREADY_ACTIVE: u16 = 3;
pub const ERR_NO_ACTIVE_SESSION: u16 = 4;
pub const ERR_BATCH_TOO_LARGE: u16 = 5;
pub const ERR_INTERNAL: u16 = 6;
pub const ERR_LIMIT_EXCEEDED: u16 = 7;
pub const ERR_BACKPRESSURE: u16 = 8;

// -- Message enums --

#[derive(Debug)]
pub enum ClientMessage {
    StartIngest { collection: String, mode: u8 },
    DocBatch { doc_count: u32, payload: Vec<u8> },
    Commit,
    Abort,
    /// Stateless upsert/delete for realtime mode.
    /// action: 0 = upsert (payload = document bytes), 1 = delete (payload = ID list)
    Upsert {
        collection: String,
        action: u8,
        doc_count: u32,
        payload: Vec<u8>,
    },
}

#[derive(Debug)]
pub enum ServerMessage {
    Ack {
        batch_seq: u64,
        docs_received: u64,
    },
    CommitResult {
        success: bool,
        docs_indexed: u64,
        build_ms: u64,
    },
    Error {
        code: u16,
        message: String,
    },
    Backpressure {
        retry_after_ms: u32,
    },
}

// ============================================================
// Encoding helpers
// ============================================================

fn write_u8(buf: &mut Vec<u8>, v: u8) {
    buf.push(v);
}

fn write_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_be_bytes());
}

fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_be_bytes());
}

fn write_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_be_bytes());
}

fn write_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_be_bytes());
}

fn write_i64(buf: &mut Vec<u8>, v: i64) {
    buf.extend_from_slice(&v.to_be_bytes());
}

fn write_f32(buf: &mut Vec<u8>, v: f32) {
    buf.extend_from_slice(&v.to_be_bytes());
}

fn write_f64(buf: &mut Vec<u8>, v: f64) {
    buf.extend_from_slice(&v.to_be_bytes());
}

fn write_str(buf: &mut Vec<u8>, s: &str) {
    write_u32(buf, s.len() as u32);
    buf.extend_from_slice(s.as_bytes());
}

fn write_str_u16(buf: &mut Vec<u8>, s: &str) {
    write_u16(buf, s.len() as u16);
    buf.extend_from_slice(s.as_bytes());
}

// ============================================================
// Decoding helpers
// ============================================================

fn read_u8(data: &[u8], offset: &mut usize) -> Result<u8, ProtocolError> {
    if *offset >= data.len() {
        return Err(ProtocolError::new("unexpected end of data reading u8"));
    }
    let v = data[*offset];
    *offset += 1;
    Ok(v)
}

fn read_u16(data: &[u8], offset: &mut usize) -> Result<u16, ProtocolError> {
    if *offset + 2 > data.len() {
        return Err(ProtocolError::new("unexpected end of data reading u16"));
    }
    let v = u16::from_be_bytes([data[*offset], data[*offset + 1]]);
    *offset += 2;
    Ok(v)
}

fn read_u32(data: &[u8], offset: &mut usize) -> Result<u32, ProtocolError> {
    if *offset + 4 > data.len() {
        return Err(ProtocolError::new("unexpected end of data reading u32"));
    }
    let v = u32::from_be_bytes([data[*offset], data[*offset + 1], data[*offset + 2], data[*offset + 3]]);
    *offset += 4;
    Ok(v)
}

fn read_u64(data: &[u8], offset: &mut usize) -> Result<u64, ProtocolError> {
    if *offset + 8 > data.len() {
        return Err(ProtocolError::new("unexpected end of data reading u64"));
    }
    let v = u64::from_be_bytes([
        data[*offset], data[*offset + 1], data[*offset + 2], data[*offset + 3],
        data[*offset + 4], data[*offset + 5], data[*offset + 6], data[*offset + 7],
    ]);
    *offset += 8;
    Ok(v)
}

fn read_i32(data: &[u8], offset: &mut usize) -> Result<i32, ProtocolError> {
    if *offset + 4 > data.len() {
        return Err(ProtocolError::new("unexpected end of data reading i32"));
    }
    let v = i32::from_be_bytes([data[*offset], data[*offset + 1], data[*offset + 2], data[*offset + 3]]);
    *offset += 4;
    Ok(v)
}

fn read_i64(data: &[u8], offset: &mut usize) -> Result<i64, ProtocolError> {
    if *offset + 8 > data.len() {
        return Err(ProtocolError::new("unexpected end of data reading i64"));
    }
    let v = i64::from_be_bytes([
        data[*offset], data[*offset + 1], data[*offset + 2], data[*offset + 3],
        data[*offset + 4], data[*offset + 5], data[*offset + 6], data[*offset + 7],
    ]);
    *offset += 8;
    Ok(v)
}

fn read_f32(data: &[u8], offset: &mut usize) -> Result<f32, ProtocolError> {
    if *offset + 4 > data.len() {
        return Err(ProtocolError::new("unexpected end of data reading f32"));
    }
    let v = f32::from_be_bytes([data[*offset], data[*offset + 1], data[*offset + 2], data[*offset + 3]]);
    *offset += 4;
    Ok(v)
}

fn read_f64(data: &[u8], offset: &mut usize) -> Result<f64, ProtocolError> {
    if *offset + 8 > data.len() {
        return Err(ProtocolError::new("unexpected end of data reading f64"));
    }
    let v = f64::from_be_bytes([
        data[*offset], data[*offset + 1], data[*offset + 2], data[*offset + 3],
        data[*offset + 4], data[*offset + 5], data[*offset + 6], data[*offset + 7],
    ]);
    *offset += 8;
    Ok(v)
}

fn read_str(data: &[u8], offset: &mut usize) -> Result<String, ProtocolError> {
    let len = read_u32(data, offset)? as usize;
    if *offset + len > data.len() {
        return Err(ProtocolError::new("unexpected end of data reading string"));
    }
    let s = std::str::from_utf8(&data[*offset..*offset + len])
        .map_err(|e| ProtocolError::new(format!("invalid UTF-8: {e}")))?;
    *offset += len;
    Ok(s.to_string())
}

fn read_str_u16(data: &[u8], offset: &mut usize) -> Result<String, ProtocolError> {
    let len = read_u16(data, offset)? as usize;
    if *offset + len > data.len() {
        return Err(ProtocolError::new("unexpected end of data reading string"));
    }
    let s = std::str::from_utf8(&data[*offset..*offset + len])
        .map_err(|e| ProtocolError::new(format!("invalid UTF-8: {e}")))?;
    *offset += len;
    Ok(s.to_string())
}

// ============================================================
// Client message serialization (used by test client)
// ============================================================

pub fn serialize_client_message(msg: &ClientMessage) -> Vec<u8> {
    let mut buf = Vec::new();
    match msg {
        ClientMessage::StartIngest { collection, mode } => {
            write_u8(&mut buf, MSG_START_INGEST);
            write_str(&mut buf, collection);
            write_u8(&mut buf, *mode);
        }
        ClientMessage::DocBatch { doc_count, payload } => {
            write_u8(&mut buf, MSG_DOC_BATCH);
            write_u32(&mut buf, *doc_count);
            buf.extend_from_slice(payload);
        }
        ClientMessage::Commit => {
            write_u8(&mut buf, MSG_COMMIT);
        }
        ClientMessage::Abort => {
            write_u8(&mut buf, MSG_ABORT);
        }
        ClientMessage::Upsert {
            collection,
            action,
            doc_count,
            payload,
        } => {
            write_u8(&mut buf, MSG_UPSERT);
            write_str(&mut buf, collection);
            write_u8(&mut buf, *action);
            write_u32(&mut buf, *doc_count);
            buf.extend_from_slice(payload);
        }
    }
    buf
}

// ============================================================
// Client message deserialization (used by server)
// ============================================================

pub fn deserialize_client_message(data: &[u8]) -> Result<ClientMessage, ProtocolError> {
    if data.is_empty() {
        return Err(ProtocolError::new("empty message"));
    }

    let mut offset = 0;
    let msg_type = read_u8(data, &mut offset)?;

    match msg_type {
        MSG_START_INGEST => {
            let collection = read_str(data, &mut offset)?;
            let mode = read_u8(data, &mut offset)?;
            Ok(ClientMessage::StartIngest { collection, mode })
        }
        MSG_DOC_BATCH => {
            let doc_count = read_u32(data, &mut offset)?;
            let payload = data[offset..].to_vec();
            Ok(ClientMessage::DocBatch { doc_count, payload })
        }
        MSG_COMMIT => Ok(ClientMessage::Commit),
        MSG_ABORT => Ok(ClientMessage::Abort),
        MSG_UPSERT => {
            let collection = read_str(data, &mut offset)?;
            let action = read_u8(data, &mut offset)?;
            let doc_count = read_u32(data, &mut offset)?;
            let payload = data[offset..].to_vec();
            Ok(ClientMessage::Upsert {
                collection,
                action,
                doc_count,
                payload,
            })
        }
        other => Err(ProtocolError::new(format!(
            "unknown client message type: 0x{other:02x}"
        ))),
    }
}

// ============================================================
// Server message serialization (used by server)
// ============================================================

pub fn serialize_server_message(msg: &ServerMessage) -> Vec<u8> {
    let mut buf = Vec::new();
    match msg {
        ServerMessage::Ack {
            batch_seq,
            docs_received,
        } => {
            write_u8(&mut buf, MSG_ACK);
            write_u64(&mut buf, *batch_seq);
            write_u64(&mut buf, *docs_received);
        }
        ServerMessage::CommitResult {
            success,
            docs_indexed,
            build_ms,
        } => {
            write_u8(&mut buf, MSG_COMMIT_RESULT);
            write_u8(&mut buf, if *success { 1 } else { 0 });
            write_u64(&mut buf, *docs_indexed);
            write_u64(&mut buf, *build_ms);
        }
        ServerMessage::Error { code, message } => {
            write_u8(&mut buf, MSG_ERROR);
            write_u16(&mut buf, *code);
            write_str(&mut buf, message);
        }
        ServerMessage::Backpressure { retry_after_ms } => {
            write_u8(&mut buf, MSG_BACKPRESSURE);
            write_u32(&mut buf, *retry_after_ms);
        }
    }
    buf
}

// ============================================================
// Server message deserialization (used by test client)
// ============================================================

pub fn deserialize_server_message(data: &[u8]) -> Result<ServerMessage, ProtocolError> {
    if data.is_empty() {
        return Err(ProtocolError::new("empty message"));
    }

    let mut offset = 0;
    let msg_type = read_u8(data, &mut offset)?;

    match msg_type {
        MSG_ACK => {
            let batch_seq = read_u64(data, &mut offset)?;
            let docs_received = read_u64(data, &mut offset)?;
            Ok(ServerMessage::Ack {
                batch_seq,
                docs_received,
            })
        }
        MSG_COMMIT_RESULT => {
            let success = read_u8(data, &mut offset)? != 0;
            let docs_indexed = read_u64(data, &mut offset)?;
            let build_ms = read_u64(data, &mut offset)?;
            Ok(ServerMessage::CommitResult {
                success,
                docs_indexed,
                build_ms,
            })
        }
        MSG_ERROR => {
            let code = read_u16(data, &mut offset)?;
            let message = read_str(data, &mut offset)?;
            Ok(ServerMessage::Error { code, message })
        }
        MSG_BACKPRESSURE => {
            let retry_after_ms = read_u32(data, &mut offset)?;
            Ok(ServerMessage::Backpressure { retry_after_ms })
        }
        other => Err(ProtocolError::new(format!(
            "unknown server message type: 0x{other:02x}"
        ))),
    }
}

// ============================================================
// Document encoding
// ============================================================

pub fn serialize_document(doc: &Document, schema: &Schema) -> Vec<u8> {
    let mut buf = Vec::new();

    // External ID (u16 length prefix)
    write_str_u16(&mut buf, &doc.external_id);

    // Language byte
    write_u8(&mut buf, doc.language.to_u8());

    // Field count
    write_u16(&mut buf, schema.fields.len() as u16);

    // Fields in schema order
    for (i, field_def) in schema.fields.iter().enumerate() {
        match doc.fields.get(i) {
            Some(FieldValue::Null) | None => {
                write_u8(&mut buf, 0); // null
            }
            Some(value) => {
                write_u8(&mut buf, 1); // present
                serialize_field_value(&mut buf, value, &field_def.field_type);
            }
        }
    }

    buf
}

fn serialize_field_value(buf: &mut Vec<u8>, value: &FieldValue, field_type: &FieldType) {
    match (field_type, value) {
        (FieldType::Text | FieldType::String, FieldValue::Text(s)) => {
            write_str(buf, s);
        }
        (FieldType::Int32, FieldValue::Int32(n)) => {
            write_i32(buf, *n);
        }
        (FieldType::Int64, FieldValue::Int64(n)) => {
            write_i64(buf, *n);
        }
        (FieldType::Float64, FieldValue::Float64(f)) => {
            write_f64(buf, *f);
        }
        (FieldType::Bool, FieldValue::Bool(b)) => {
            write_u8(buf, if *b { 1 } else { 0 });
        }
        (FieldType::GeoPoint, FieldValue::GeoPoint { lat, lng }) => {
            write_f64(buf, *lat);
            write_f64(buf, *lng);
        }
        (FieldType::StringArray | FieldType::StringStar, FieldValue::StringArray(arr)) => {
            write_u16(buf, arr.len() as u16);
            for item in arr {
                match item {
                    None => write_u8(buf, 0),
                    Some(s) => { write_u8(buf, 1); write_str(buf, s); }
                }
            }
        }
        (FieldType::BoolArray, FieldValue::BoolArray(arr)) => {
            write_u16(buf, arr.len() as u16);
            for item in arr {
                match item {
                    None => write_u8(buf, 0),
                    Some(b) => { write_u8(buf, 1); write_u8(buf, if *b { 1 } else { 0 }); }
                }
            }
        }
        (FieldType::GeoPointArray, FieldValue::GeoPointArray(arr)) => {
            write_u16(buf, arr.len() as u16);
            for (lat, lng) in arr {
                write_f64(buf, *lat);
                write_f64(buf, *lng);
            }
        }
        (FieldType::Int32Array, FieldValue::Int32Array(arr)) => {
            write_u16(buf, arr.len() as u16);
            for item in arr {
                match item {
                    None => write_u8(buf, 0),
                    Some(n) => { write_u8(buf, 1); write_i32(buf, *n); }
                }
            }
        }
        (FieldType::Int64Array, FieldValue::Int64Array(arr)) => {
            write_u16(buf, arr.len() as u16);
            for item in arr {
                match item {
                    None => write_u8(buf, 0),
                    Some(n) => { write_u8(buf, 1); write_i64(buf, *n); }
                }
            }
        }
        (FieldType::FloatArray { .. }, FieldValue::FloatArray(arr)) => {
            write_u16(buf, arr.len() as u16);
            for f in arr {
                write_f32(buf, *f);
            }
        }
        _ => {} // type mismatch — skip (shouldn't happen with valid data)
    }
}

pub fn deserialize_document(
    data: &[u8],
    schema: &Schema,
    offset: &mut usize,
    doc_id: DocId,
) -> Result<Document, ProtocolError> {
    // External ID
    let external_id = read_str_u16(data, offset)?;

    // Language
    let lang_byte = read_u8(data, offset)?;
    let language = Language::from_u8(lang_byte);

    // Field count
    let field_count = read_u16(data, offset)? as usize;
    if field_count != schema.fields.len() {
        return Err(ProtocolError::new(format!(
            "field count mismatch: got {field_count}, schema has {}",
            schema.fields.len()
        )));
    }

    let mut fields = crate::schema::SparseFields::with_capacity(field_count);
    for (field_idx, field_def) in schema.fields.iter().enumerate() {
        let presence = read_u8(data, offset)?;
        if presence == 0 {
            // Null — not stored in SparseFields
        } else {
            let value = deserialize_field_value(data, offset, &field_def.field_type)
                .map_err(|e| ProtocolError::new(format!(
                    "field[{}] '{}' (expected {:?}): {}",
                    field_idx, field_def.name, field_def.field_type, e
                )))?;
            fields.push(field_def.id, value);
        }
    }

    Ok(Document {
        id: doc_id,
        external_id,
        language,
        fields,
        record: None,
    })
}

fn deserialize_field_value(
    data: &[u8],
    offset: &mut usize,
    field_type: &FieldType,
) -> Result<FieldValue, ProtocolError> {
    match field_type {
        FieldType::Text | FieldType::String => {
            let s = read_str(data, offset)?;
            Ok(FieldValue::Text(s))
        }
        FieldType::Int32 => {
            let n = read_i32(data, offset)?;
            Ok(FieldValue::Int32(n))
        }
        FieldType::Int64 => {
            let n = read_i64(data, offset)?;
            Ok(FieldValue::Int64(n))
        }
        FieldType::Float64 => {
            let f = read_f64(data, offset)?;
            Ok(FieldValue::Float64(f))
        }
        FieldType::Bool => {
            let b = read_u8(data, offset)?;
            Ok(FieldValue::Bool(b != 0))
        }
        FieldType::GeoPoint => {
            let lat = read_f64(data, offset)?;
            let lng = read_f64(data, offset)?;
            Ok(FieldValue::GeoPoint { lat, lng })
        }
        FieldType::Object | FieldType::ObjectArray | FieldType::Auto => {
            return Err(ProtocolError::new(
                "object/object[]/auto fields carry no direct value",
            ));
        }
        FieldType::BoolArray => {
            let count = read_u16(data, offset)? as usize;
            let mut arr = Vec::with_capacity(count);
            for _ in 0..count {
                if read_u8(data, offset)? == 0 {
                    arr.push(None);
                } else {
                    arr.push(Some(read_u8(data, offset)? != 0));
                }
            }
            Ok(FieldValue::BoolArray(arr))
        }
        FieldType::GeoPointArray => {
            let count = read_u16(data, offset)? as usize;
            let mut arr = Vec::with_capacity(count);
            for _ in 0..count {
                let lat = read_f64(data, offset)?;
                let lng = read_f64(data, offset)?;
                arr.push((lat, lng));
            }
            Ok(FieldValue::GeoPointArray(arr))
        }
        FieldType::StringArray | FieldType::StringStar => {
            let count = read_u16(data, offset)? as usize;
            let mut arr = Vec::with_capacity(count);
            for _ in 0..count {
                if read_u8(data, offset)? == 0 {
                    arr.push(None);
                } else {
                    arr.push(Some(read_str(data, offset)?));
                }
            }
            Ok(FieldValue::StringArray(arr))
        }
        FieldType::Int32Array => {
            let count = read_u16(data, offset)? as usize;
            let mut arr = Vec::with_capacity(count);
            for _ in 0..count {
                if read_u8(data, offset)? == 0 {
                    arr.push(None);
                } else {
                    arr.push(Some(read_i32(data, offset)?));
                }
            }
            Ok(FieldValue::Int32Array(arr))
        }
        FieldType::Int64Array => {
            let count = read_u16(data, offset)? as usize;
            let mut arr = Vec::with_capacity(count);
            for _ in 0..count {
                if read_u8(data, offset)? == 0 {
                    arr.push(None);
                } else {
                    arr.push(Some(read_i64(data, offset)?));
                }
            }
            Ok(FieldValue::Int64Array(arr))
        }
        FieldType::FloatArray { .. } => {
            let count = read_u16(data, offset)? as usize;
            let mut arr = Vec::with_capacity(count);
            for _ in 0..count {
                arr.push(read_f32(data, offset)?);
            }
            Ok(FieldValue::FloatArray(arr))
        }
    }
}

/// Decode a list of external IDs from a delete payload.
/// Wire format: repeated [u16 id_len][id_bytes].
pub fn decode_delete_ids(
    payload: &[u8],
    count: u32,
) -> Result<Vec<String>, ProtocolError> {
    let mut offset = 0;
    let mut ids = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let id = read_str_u16(payload, &mut offset)?;
        ids.push(id);
    }
    Ok(ids)
}

/// Encode a list of external IDs into a delete payload.
/// Wire format: repeated [u16 id_len][id_bytes].
pub fn encode_delete_ids(ids: &[String]) -> Vec<u8> {
    let mut buf = Vec::new();
    for id in ids {
        write_str_u16(&mut buf, id);
    }
    buf
}

/// Encode multiple documents into a DocBatch payload.
pub fn encode_doc_batch(docs: &[Document], schema: &Schema) -> ClientMessage {
    let mut payload = Vec::new();
    for doc in docs {
        payload.extend(serialize_document(doc, schema));
    }
    ClientMessage::DocBatch {
        doc_count: docs.len() as u32,
        payload,
    }
}

/// Decode documents from a DocBatch payload.
/// For batches >100 docs, uses parallel deserialization via rayon.
pub fn decode_doc_batch(
    payload: &[u8],
    doc_count: u32,
    schema: &Schema,
    start_doc_id: DocId,
) -> Result<Vec<Document>, ProtocolError> {
    let count = doc_count as usize;

    if count > 100 {
        // Phase 1: single-pass offset discovery (sequential, no allocation)
        let mut offsets = Vec::with_capacity(count);
        let mut offset = 0;
        for _ in 0..doc_count {
            offsets.push(offset);
            skip_document(payload, schema, &mut offset)?;
        }

        // Phase 2: parallel deserialization using discovered offsets
        use rayon::prelude::*;
        offsets
            .into_par_iter()
            .enumerate()
            .map(|(i, start)| {
                let mut off = start;
                deserialize_document(payload, schema, &mut off, start_doc_id + i as u32)
            })
            .collect()
    } else {
        let mut offset = 0;
        let mut docs = Vec::with_capacity(count);
        for i in 0..doc_count {
            let doc = deserialize_document(payload, schema, &mut offset, start_doc_id + i)?;
            docs.push(doc);
        }
        Ok(docs)
    }
}

/// Decode a segment of documents from a payload starting at a byte offset.
/// Returns the decoded documents and the number of bytes consumed from the payload.
/// Documents are assigned IDs starting at `start_doc_id`.
pub fn decode_doc_segment(
    payload: &[u8],
    byte_offset: usize,
    doc_count: u32,
    schema: &Schema,
    start_doc_id: DocId,
) -> Result<(Vec<Document>, usize), ProtocolError> {
    // Yield once at entry. The first work `decode_doc_batch` does is
    // `Vec::with_capacity(100K)` which under heavy allocator
    // fragmentation can take 20-30 s on its own — long enough to
    // trip the YFP-FREEZE watchdog before the per-doc yields
    // (inside the loop) ever run.
    let sub_payload = &payload[byte_offset..];
    let docs = decode_doc_batch(sub_payload, doc_count, schema, start_doc_id)?;

    // Compute bytes consumed by skipping `doc_count` docs from the same start
    let mut skip_offset = byte_offset;
    for _ in 0..doc_count {
        skip_document(payload, schema, &mut skip_offset)?;
    }
    let bytes_consumed = skip_offset - byte_offset;

    Ok((docs, bytes_consumed))
}

/// Decode documents from a large payload in chunks for better cache locality.
/// Each chunk gets its own offset-discovery + parallel-decode pass, avoiding
/// a single sequential scan of the entire payload.
pub fn decode_doc_batch_chunked(
    payload: &[u8],
    doc_count: u32,
    schema: &Schema,
    chunk_size: u32,
) -> Result<Vec<Document>, ProtocolError> {
    let mut all_docs = Vec::with_capacity(doc_count as usize);
    let mut global_offset = 0usize;
    let mut remaining = doc_count;

    while remaining > 0 {
        let batch = remaining.min(chunk_size);
        let batch_docs = decode_doc_batch(&payload[global_offset..], batch, schema, 0)?;

        // Advance global offset by skipping `batch` documents
        for _ in 0..batch {
            skip_document(payload, schema, &mut global_offset)?;
        }

        all_docs.extend(batch_docs);
        remaining -= batch;
    }

    Ok(all_docs)
}

/// Skip past a document in the payload without allocating.
/// Advances `offset` to the start of the next document.
/// Public alias for `skip_document` so the streaming builder can pre-walk
/// a payload to find segment byte boundaries without decoding documents.
pub fn skip_document_pub(
    data: &[u8],
    schema: &Schema,
    offset: &mut usize,
) -> Result<(), ProtocolError> {
    skip_document(data, schema, offset)
}

fn skip_document(
    data: &[u8],
    schema: &Schema,
    offset: &mut usize,
) -> Result<(), ProtocolError> {
    // External ID (u16 length-prefixed string)
    let id_len = read_u16(data, offset)? as usize;
    if *offset + id_len > data.len() {
        return Err(ProtocolError::new("unexpected end of data skipping external_id"));
    }
    *offset += id_len;

    // Language byte
    *offset += 1;
    if *offset > data.len() {
        return Err(ProtocolError::new("unexpected end of data skipping language"));
    }

    // Field count
    let field_count = read_u16(data, offset)? as usize;
    if field_count != schema.fields.len() {
        return Err(ProtocolError::new(format!(
            "field count mismatch: got {field_count}, schema has {}",
            schema.fields.len()
        )));
    }

    // Skip each field
    for field_def in &schema.fields {
        let presence = read_u8(data, offset)?;
        if presence != 0 {
            skip_field_value(data, offset, &field_def.field_type)?;
        }
    }

    Ok(())
}

/// Skip past a field value without allocating.
fn skip_field_value(
    data: &[u8],
    offset: &mut usize,
    field_type: &FieldType,
) -> Result<(), ProtocolError> {
    match field_type {
        FieldType::Text | FieldType::String => {
            let len = read_u32(data, offset)? as usize;
            if *offset + len > data.len() {
                return Err(ProtocolError::new("unexpected end of data skipping string"));
            }
            *offset += len;
        }
        FieldType::Int32 => { *offset += 4; }
        FieldType::Int64 => { *offset += 8; }
        FieldType::Float64 => { *offset += 8; }
        FieldType::Bool => { *offset += 1; }
        FieldType::GeoPoint => { *offset += 16; }
        FieldType::Object | FieldType::ObjectArray | FieldType::Auto => {
            return Err(ProtocolError::new(
                "object/object[]/auto fields carry no direct value",
            ));
        }
        FieldType::StringArray | FieldType::StringStar => {
            let count = read_u16(data, offset)? as usize;
            for _ in 0..count {
                let flag = read_u8(data, offset)?;
                if flag != 0 {
                    let len = read_u32(data, offset)? as usize;
                    if *offset + len > data.len() {
                        return Err(ProtocolError::new("unexpected end of data skipping string array element"));
                    }
                    *offset += len;
                }
            }
        }
        FieldType::BoolArray => {
            let count = read_u16(data, offset)? as usize;
            for _ in 0..count {
                if read_u8(data, offset)? != 0 {
                    *offset += 1;
                }
            }
        }
        FieldType::GeoPointArray => {
            let count = read_u16(data, offset)? as usize;
            *offset += count * 16;
        }
        FieldType::Int32Array => {
            let count = read_u16(data, offset)? as usize;
            for _ in 0..count {
                let flag = read_u8(data, offset)?;
                if flag != 0 { *offset += 4; }
            }
        }
        FieldType::Int64Array => {
            let count = read_u16(data, offset)? as usize;
            for _ in 0..count {
                let flag = read_u8(data, offset)?;
                if flag != 0 { *offset += 8; }
            }
        }
        FieldType::FloatArray { .. } => {
            let count = read_u16(data, offset)? as usize;
            *offset += count * 4;
        }
    }
    if *offset > data.len() {
        return Err(ProtocolError::new("unexpected end of data skipping field value"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::FieldDef;

    fn test_schema() -> Schema {
        let mut schema = Schema {
            name: "test".into(),
            fields: vec![
                FieldDef {
                    name: "title".into(),
                    field_type: FieldType::String,
                    facet: false,
                    index: true,
                    optional: false,
                    sort: false,
                    stem: true,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "price".into(),
                    field_type: FieldType::Int32,
                    facet: false,
                    index: true,
                    optional: true,
                    sort: true,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "rating".into(),
                    field_type: FieldType::Float64,
                    facet: false,
                    index: true,
                    optional: true,
                    sort: true,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "in_stock".into(),
                    field_type: FieldType::Bool,
                    facet: false,
                    index: true,
                    optional: true,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "tags".into(),
                    field_type: FieldType::StringArray,
                    facet: true,
                    index: true,
                    optional: true,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "location".into(),
                    field_type: FieldType::GeoPoint,
                    facet: false,
                    index: false,
                    optional: true,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
            ],
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: Language::default(),
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        schema.assign_field_ids();
        schema
    }

    #[test]
    fn document_roundtrip_all_types() {
        let schema = test_schema();
        let doc = Document {
            id: 42,
            external_id: "prod-1".into(),
            language: Language::Swedish,
            fields: vec![
                FieldValue::Text("Rust Programming".into()),
                FieldValue::Int32(299),
                FieldValue::Float64(4.5),
                FieldValue::Bool(true),
                FieldValue::StringArray(vec![Some("tech".into()), Some("books".into())]),
                FieldValue::GeoPoint {
                    lat: 59.3293,
                    lng: 18.0686,
                },
            ].into(),
            record: None,
        };

        let bytes = serialize_document(&doc, &schema);
        let mut offset = 0;
        let decoded = deserialize_document(&bytes, &schema, &mut offset, 42).unwrap();

        assert_eq!(decoded.external_id, "prod-1");
        assert_eq!(decoded.language, Language::Swedish);
        assert_eq!(decoded.id, 42);
        assert!(matches!(&decoded.fields.get(0).unwrap(), FieldValue::Text(s) if s == "Rust Programming"));
        assert!(matches!(decoded.fields.get(1).unwrap(), FieldValue::Int32(299)));
        assert!(matches!(decoded.fields.get(2).unwrap(), FieldValue::Float64(f) if (f - 4.5).abs() < 0.001));
        assert!(matches!(decoded.fields.get(3).unwrap(), FieldValue::Bool(true)));
        match &decoded.fields.get(4).unwrap() {
            FieldValue::StringArray(v) => {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0].as_deref(), Some("tech"));
                assert_eq!(v[1].as_deref(), Some("books"));
            }
            _ => panic!("expected StringArray"),
        }
        assert!(
            matches!(decoded.fields.get(5).unwrap(), FieldValue::GeoPoint { lat, lng } if (lat - 59.3293).abs() < 0.0001 && (lng - 18.0686).abs() < 0.0001)
        );
        assert_eq!(offset, bytes.len());
    }

    #[test]
    fn document_roundtrip_with_nulls() {
        let schema = test_schema();
        let doc = Document {
            id: 0,
            external_id: "1".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("Hello".into()),
                FieldValue::Null,
                FieldValue::Null,
                FieldValue::Null,
                FieldValue::Null,
                FieldValue::Null,
            ].into(),
            record: None,
        };

        let bytes = serialize_document(&doc, &schema);
        let mut offset = 0;
        let decoded = deserialize_document(&bytes, &schema, &mut offset, 0).unwrap();

        assert!(matches!(&decoded.fields.get(0).unwrap(), FieldValue::Text(s) if s == "Hello"));
        for i in 1..6 {
            assert!(
                decoded.fields.get(i).is_none(),
                "field {i} should be absent (null)"
            );
        }
    }

    #[test]
    fn doc_batch_roundtrip() {
        let schema = test_schema();
        let docs = vec![
            Document {
                id: 0,
                external_id: "a".into(),
                language: Language::English,
                fields: vec![
                    FieldValue::Text("First".into()),
                    FieldValue::Int32(10),
                    FieldValue::Null,
                    FieldValue::Bool(true),
                    FieldValue::Null,
                    FieldValue::Null,
                ].into(),
                record: None,
            },
            Document {
                id: 1,
                external_id: "b".into(),
                language: Language::Finnish,
                fields: vec![
                    FieldValue::Text("Second".into()),
                    FieldValue::Int32(20),
                    FieldValue::Float64(3.14),
                    FieldValue::Bool(false),
                    FieldValue::StringArray(vec![Some("x".into())]),
                    FieldValue::Null,
                ].into(),
                record: None,
            },
        ];

        let batch = encode_doc_batch(&docs, &schema);
        if let ClientMessage::DocBatch { doc_count, payload } = &batch {
            assert_eq!(*doc_count, 2);
            let decoded = decode_doc_batch(payload, *doc_count, &schema, 0).unwrap();
            assert_eq!(decoded.len(), 2);
            assert_eq!(decoded[0].external_id, "a");
            assert_eq!(decoded[1].external_id, "b");
            assert_eq!(decoded[1].language, Language::Finnish);
        } else {
            panic!("expected DocBatch");
        }
    }

    #[test]
    fn start_ingest_roundtrip() {
        let msg = ClientMessage::StartIngest {
            collection: "products".into(),
            mode: 0,
        };
        let bytes = serialize_client_message(&msg);
        let decoded = deserialize_client_message(&bytes).unwrap();
        match decoded {
            ClientMessage::StartIngest { collection, mode } => {
                assert_eq!(collection, "products");
                assert_eq!(mode, 0);
            }
            _ => panic!("expected StartIngest"),
        }
    }

    #[test]
    fn commit_roundtrip() {
        let bytes = serialize_client_message(&ClientMessage::Commit);
        let decoded = deserialize_client_message(&bytes).unwrap();
        assert!(matches!(decoded, ClientMessage::Commit));
    }

    #[test]
    fn abort_roundtrip() {
        let bytes = serialize_client_message(&ClientMessage::Abort);
        let decoded = deserialize_client_message(&bytes).unwrap();
        assert!(matches!(decoded, ClientMessage::Abort));
    }

    #[test]
    fn ack_roundtrip() {
        let msg = ServerMessage::Ack {
            batch_seq: 5,
            docs_received: 1000,
        };
        let bytes = serialize_server_message(&msg);
        let decoded = deserialize_server_message(&bytes).unwrap();
        match decoded {
            ServerMessage::Ack {
                batch_seq,
                docs_received,
            } => {
                assert_eq!(batch_seq, 5);
                assert_eq!(docs_received, 1000);
            }
            _ => panic!("expected Ack"),
        }
    }

    #[test]
    fn commit_result_roundtrip() {
        let msg = ServerMessage::CommitResult {
            success: true,
            docs_indexed: 5000,
            build_ms: 42,
        };
        let bytes = serialize_server_message(&msg);
        let decoded = deserialize_server_message(&bytes).unwrap();
        match decoded {
            ServerMessage::CommitResult {
                success,
                docs_indexed,
                build_ms,
            } => {
                assert!(success);
                assert_eq!(docs_indexed, 5000);
                assert_eq!(build_ms, 42);
            }
            _ => panic!("expected CommitResult"),
        }
    }

    #[test]
    fn error_roundtrip() {
        let msg = ServerMessage::Error {
            code: ERR_COLLECTION_NOT_FOUND,
            message: "Collection `foo` not found.".into(),
        };
        let bytes = serialize_server_message(&msg);
        let decoded = deserialize_server_message(&bytes).unwrap();
        match decoded {
            ServerMessage::Error { code, message } => {
                assert_eq!(code, ERR_COLLECTION_NOT_FOUND);
                assert_eq!(message, "Collection `foo` not found.");
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn unknown_message_type_rejected() {
        let data = [0xFF];
        assert!(deserialize_client_message(&data).is_err());
        assert!(deserialize_server_message(&data).is_err());
    }

    #[test]
    fn empty_message_rejected() {
        assert!(deserialize_client_message(&[]).is_err());
        assert!(deserialize_server_message(&[]).is_err());
    }

    #[test]
    fn document_roundtrip_nullable_array_elements() {
        let mut schema = Schema {
            name: "test".into(),
            fields: vec![
                FieldDef {
                    name: "names".into(),
                    field_type: FieldType::StringArray,
                    facet: false, index: true, optional: false, sort: false,
                    stem: false, locale: None, num_dim: None, vec_dist: None, id: 0, reference: None, drop: false,
                    infix: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "scores".into(),
                    field_type: FieldType::Int32Array,
                    facet: false, index: true, optional: false, sort: false,
                    stem: false, locale: None, num_dim: None, vec_dist: None, id: 0, reference: None, drop: false,
                    infix: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "big_ids".into(),
                    field_type: FieldType::Int64Array,
                    facet: false, index: true, optional: false, sort: false,
                    stem: false, locale: None, num_dim: None, vec_dist: None, id: 0, reference: None, drop: false,
                    infix: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
            ],
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: Language::default(),
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        schema.assign_field_ids();

        let doc = Document {
            id: 0,
            external_id: "n1".into(),
            language: Language::English,
            fields: vec![
                FieldValue::StringArray(vec![Some("hello".into()), None, Some("world".into())]),
                FieldValue::Int32Array(vec![Some(10), None, Some(30)]),
                FieldValue::Int64Array(vec![None, Some(200)]),
            ].into(),
            record: None,
        };

        let bytes = serialize_document(&doc, &schema);
        let mut offset = 0;
        let decoded = deserialize_document(&bytes, &schema, &mut offset, 0).unwrap();

        match &decoded.fields.get(0).unwrap() {
            FieldValue::StringArray(v) => {
                assert_eq!(v.len(), 3);
                assert_eq!(v[0].as_deref(), Some("hello"));
                assert!(v[1].is_none());
                assert_eq!(v[2].as_deref(), Some("world"));
            }
            _ => panic!("expected StringArray"),
        }
        match &decoded.fields.get(1).unwrap() {
            FieldValue::Int32Array(v) => {
                assert_eq!(v.len(), 3);
                assert_eq!(v[0], Some(10));
                assert!(v[1].is_none());
                assert_eq!(v[2], Some(30));
            }
            _ => panic!("expected Int32Array"),
        }
        match &decoded.fields.get(2).unwrap() {
            FieldValue::Int64Array(v) => {
                assert_eq!(v.len(), 2);
                assert!(v[0].is_none());
                assert_eq!(v[1], Some(200));
            }
            _ => panic!("expected Int64Array"),
        }
        assert_eq!(offset, bytes.len());
    }
}
