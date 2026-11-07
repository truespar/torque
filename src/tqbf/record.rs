//! Document records (spec §5.2): a compact binary tree keyed by registry ids.
//!
//! ```text
//! record := varint(id_len) id_bytes object
//! object := varint(n) entry{n}                 entries in key byte order
//! entry  := varint(field_id << 4 | tag) payload
//! ```
//! Integers and floats stay distinct and floats keep their exact bits, so a record renders back to
//! the same JSON Typesense stores. Empty arrays are kept; absent keys are absent.

use super::registry::{FieldRegistry, RegistryFull, NO_PARENT};
use serde_json::{Map, Number, Value};

pub const TAG_INT: u8 = 0;
pub const TAG_FLOAT: u8 = 1;
pub const TAG_STR: u8 = 2;
pub const TAG_FALSE: u8 = 3;
pub const TAG_TRUE: u8 = 4;
pub const TAG_ARRAY: u8 = 5;
pub const TAG_OBJECT: u8 = 6;
pub const TAG_NULL: u8 = 7;
pub const TAG_U64: u8 = 8;
/// Array element type only: one byte (0/1) per element.
pub const TAG_BOOL: u8 = 9;
/// Array element type only: every element carries its own tag byte.
pub const TAG_MIXED: u8 = 15;

#[derive(Debug)]
pub enum RecordError {
    NotAnObject,
    /// `encode_known` met a path the registry does not hold.
    UnknownPath,
    MissingId,
    Registry(RegistryFull),
    Corrupt(&'static str),
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnObject => f.write_str("document must be a JSON object"),
            Self::UnknownPath => f.write_str("document has a path the registry does not hold"),
            Self::MissingId => f.write_str("document has no string `id`"),
            Self::Registry(e) => e.fmt(f),
            Self::Corrupt(what) => write!(f, "corrupt record: {what}"),
        }
    }
}

impl std::error::Error for RecordError {}

impl From<RegistryFull> for RecordError {
    fn from(e: RegistryFull) -> Self {
        Self::Registry(e)
    }
}

// -- varints ---------------------------------------------------------------------------------

#[inline]
pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline]
fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

// -- encoding --------------------------------------------------------------------------------

/// Maps (parent id, key) to a field id while encoding.
trait PathIds {
    fn id(&mut self, parent: u16, name: &str) -> Result<u16, RecordError>;
}

struct Adding<'a>(&'a mut FieldRegistry);

impl PathIds for Adding<'_> {
    fn id(&mut self, parent: u16, name: &str) -> Result<u16, RecordError> {
        Ok(self.0.ensure(parent, name)?)
    }
}

struct KnownOnly<'a>(&'a FieldRegistry);

impl PathIds for KnownOnly<'_> {
    fn id(&mut self, parent: u16, name: &str) -> Result<u16, RecordError> {
        self.0.lookup(parent, name).ok_or(RecordError::UnknownPath)
    }
}

/// Encode a JSON document. Paths not yet in `reg` are added as stored-only entries.
pub fn encode(doc: &Value, id: &str, reg: &mut FieldRegistry) -> Result<Vec<u8>, RecordError> {
    encode_with(doc, id, &mut Adding(reg))
}

/// Encode with a read-only registry; fails with `UnknownPath` when the document has a new path.
pub fn encode_known(doc: &Value, id: &str, reg: &FieldRegistry) -> Result<Vec<u8>, RecordError> {
    encode_with(doc, id, &mut KnownOnly(reg))
}

fn encode_with(doc: &Value, id: &str, ids: &mut dyn PathIds) -> Result<Vec<u8>, RecordError> {
    let obj = doc.as_object().ok_or(RecordError::NotAnObject)?;
    let mut out = Vec::with_capacity(256);
    put_varint(&mut out, id.len() as u64);
    out.extend_from_slice(id.as_bytes());
    encode_object(obj, NO_PARENT, true, ids, &mut out)?;
    Ok(out)
}

/// The document's `id` the way Typesense accepts it (string; numbers are not ids).
pub fn document_id(doc: &Value) -> Option<&str> {
    doc.get("id").and_then(Value::as_str)
}

fn encode_object(
    obj: &Map<String, Value>,
    parent: u16,
    top_level: bool,
    ids: &mut dyn PathIds,
    out: &mut Vec<u8>,
) -> Result<(), RecordError> {
    let mut keys: Vec<&String> = obj.keys().filter(|k| !(top_level && k.as_str() == "id")).collect();
    keys.sort_unstable();
    put_varint(out, keys.len() as u64);
    for key in keys {
        let value = &obj[key];
        let fid = ids.id(parent, key)?;
        put_varint(out, (u64::from(fid) << 4) | u64::from(scalar_tag(value)));
        encode_payload(value, fid, ids, out)?;
    }
    Ok(())
}

fn scalar_tag(v: &Value) -> u8 {
    match v {
        Value::Null => TAG_NULL,
        Value::Bool(false) => TAG_FALSE,
        Value::Bool(true) => TAG_TRUE,
        Value::Number(n) if n.is_i64() => TAG_INT,
        Value::Number(n) if n.is_u64() => TAG_U64,
        Value::Number(_) => TAG_FLOAT,
        Value::String(_) => TAG_STR,
        Value::Array(_) => TAG_ARRAY,
        Value::Object(_) => TAG_OBJECT,
    }
}

fn element_tag(v: &Value) -> u8 {
    match v {
        Value::Bool(_) => TAG_BOOL,
        other => scalar_tag(other),
    }
}

/// Payload of a value whose tag is already written. `fid` is the id of the key the value hangs
/// under; objects inside arrays use it as their parent.
fn encode_payload(v: &Value, fid: u16, ids: &mut dyn PathIds, out: &mut Vec<u8>) -> Result<(), RecordError> {
    match v {
        Value::Null | Value::Bool(_) => {}
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                put_varint(out, zigzag(i));
            } else if let Some(u) = n.as_u64() {
                put_varint(out, u);
            } else {
                out.extend_from_slice(&n.as_f64().unwrap_or(0.0).to_bits().to_le_bytes());
            }
        }
        Value::String(s) => {
            put_varint(out, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Array(arr) => {
            put_varint(out, arr.len() as u64);
            if let Some(first) = arr.first() {
                let t = element_tag(first);
                let uniform = arr.iter().all(|e| element_tag(e) == t);
                if uniform {
                    out.push(t);
                    for e in arr {
                        if t == TAG_BOOL {
                            out.push(u8::from(e.as_bool() == Some(true)));
                        } else {
                            encode_payload(e, fid, ids, out)?;
                        }
                    }
                } else {
                    out.push(TAG_MIXED);
                    for e in arr {
                        out.push(scalar_tag(e));
                        encode_payload(e, fid, ids, out)?;
                    }
                }
            }
        }
        Value::Object(obj) => encode_object(obj, fid, false, ids, out)?,
    }
    Ok(())
}

// -- decoding --------------------------------------------------------------------------------

/// Zero-copy cursor over record bytes.
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    #[inline]
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[inline]
    pub fn varint(&mut self) -> Result<u64, RecordError> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = *self.buf.get(self.pos).ok_or(RecordError::Corrupt("varint past end"))?;
            self.pos += 1;
            v |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                return Err(RecordError::Corrupt("varint too long"));
            }
        }
    }

    #[inline]
    pub fn byte(&mut self) -> Result<u8, RecordError> {
        let b = *self.buf.get(self.pos).ok_or(RecordError::Corrupt("byte past end"))?;
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], RecordError> {
        let s = self.buf.get(self.pos..self.pos + n).ok_or(RecordError::Corrupt("bytes past end"))?;
        self.pos += n;
        Ok(s)
    }

    #[inline]
    pub fn str(&mut self) -> Result<&'a str, RecordError> {
        let n = self.varint()? as usize;
        std::str::from_utf8(self.bytes(n)?).map_err(|_| RecordError::Corrupt("invalid UTF-8"))
    }

    /// Bytes not read yet.
    #[inline]
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    #[inline]
    pub fn f64(&mut self) -> Result<f64, RecordError> {
        Ok(f64::from_bits(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap())))
    }
}

/// The external id stored at the start of a record.
pub fn record_id(rec: &[u8]) -> Result<&str, RecordError> {
    Cursor::new(rec).str()
}

/// Decode a record into the JSON value Typesense would return for it (`id` included).
/// `serde_json::Map` is a BTreeMap here, so keys come out in byte order, as nlohmann writes them.
pub fn to_value(rec: &[u8], reg: &FieldRegistry) -> Result<Value, RecordError> {
    let mut c = Cursor::new(rec);
    let id = c.str()?;
    let mut map = decode_object(&mut c, reg)?;
    map.insert("id".to_string(), Value::String(id.to_string()));
    Ok(Value::Object(map))
}

fn decode_object(c: &mut Cursor<'_>, reg: &FieldRegistry) -> Result<Map<String, Value>, RecordError> {
    let n = c.varint()? as usize;
    let mut map = Map::new();
    for _ in 0..n {
        let key = c.varint()?;
        let fid = (key >> 4) as usize;
        if fid >= reg.len() {
            return Err(RecordError::Corrupt("field id outside registry"));
        }
        let value = decode_payload(c, (key & 0xF) as u8, reg)?;
        map.insert(reg.entry(fid as u16).name.to_string(), value);
    }
    Ok(map)
}

fn decode_payload(c: &mut Cursor<'_>, tag: u8, reg: &FieldRegistry) -> Result<Value, RecordError> {
    Ok(match tag {
        TAG_INT => Value::Number(unzigzag(c.varint()?).into()),
        TAG_U64 => Value::Number(c.varint()?.into()),
        TAG_FLOAT => Value::Number(Number::from_f64(c.f64()?).ok_or(RecordError::Corrupt("non-finite float"))?),
        TAG_STR => Value::String(c.str()?.to_string()),
        TAG_FALSE => Value::Bool(false),
        TAG_TRUE => Value::Bool(true),
        TAG_NULL => Value::Null,
        TAG_BOOL => Value::Bool(c.byte()? != 0),
        TAG_OBJECT => Value::Object(decode_object(c, reg)?),
        TAG_ARRAY => {
            let n = c.varint()? as usize;
            let mut arr = Vec::with_capacity(n.min(4096));
            if n > 0 {
                let t = c.byte()?;
                for _ in 0..n {
                    let et = if t == TAG_MIXED { c.byte()? } else { t };
                    arr.push(decode_payload(c, et, reg)?);
                }
            }
            Value::Array(arr)
        }
        _ => return Err(RecordError::Corrupt("unknown tag")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_types_order_and_empty_arrays() {
        let src = r#"{"id":"42","b":[],"a":{"z":1,"y":[{"k":"v"},{"k":"w","n":2.5}]},
            "f":40.0,"i":-7,"u":18446744073709551615,"t":true,"s":"åäö \"q\"","m":[1,"x",null,false],
            "g":[[59.31337,18.0939]],"bools":[true,false],"nul":null,"e":{}}"#;
        let doc: Value = serde_json::from_str(src).unwrap();
        let mut reg = FieldRegistry::new();
        let rec = encode(&doc, document_id(&doc).unwrap(), &mut reg).unwrap();
        assert_eq!(record_id(&rec).unwrap(), "42");
        let back = to_value(&rec, &reg).unwrap();
        assert_eq!(back, doc);
        assert_eq!(serde_json::to_string(&back).unwrap(), serde_json::to_string(&doc).unwrap());
        assert!(back["f"].is_f64() && back["i"].is_i64() && back["u"].is_u64());
        // Same registry, second document: no new ids for known paths.
        let before = reg.len();
        assert_eq!(encode_known(&doc, "42", &reg).unwrap(), rec);
        assert_eq!(reg.len(), before);
        let other: Value = serde_json::from_str(r#"{"id":"1","new":1}"#).unwrap();
        assert!(matches!(encode_known(&other, "1", &reg), Err(RecordError::UnknownPath)));
    }
}
