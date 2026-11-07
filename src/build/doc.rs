//! A record's typed field values, read in place from the record bytes exactly as a JSONL import
//! reads the same document (`ingest::typed_fields` over `flatten_json_object`):
//!
//! - nested objects flatten to dotted keys;
//! - an array whose first element is an object pivots into one array per key of its elements,
//!   null where an element lacks the key (values inside are taken as they are, not flattened);
//! - of two equal flattened keys the later one in key order wins;
//! - values are type-checked per schema field in schema order, and a document fails with the
//!   message the import gives.
//!
//! Nothing is copied: strings are (offset, length) in the record, arrays ranges of a scratch
//! buffer reused for every document.

use crate::schema::{FieldDef, FieldType, Language, Schema};
use crate::tqbf::record::{
    Cursor, RecordError, TAG_ARRAY, TAG_BOOL, TAG_FALSE, TAG_FLOAT, TAG_INT, TAG_MIXED, TAG_NULL, TAG_OBJECT, TAG_STR, TAG_TRUE,
    TAG_U64,
};
use crate::tqbf::registry::{FieldRegistry, NO_PARENT};
use rustc_hash::FxHashMap;

const NONE: u32 = u32::MAX;

/// A field value of one document. Strings are byte ranges of the record; the plural variants
/// ranges of the scratch vectors of `DocValues`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Typed {
    Absent,
    Str(u32, u32),
    Strs(u32, u32),
    Int(i64),
    Float(f64),
    Bool(bool),
    Ints(u32, u32),
    Bools(u32, u32),
    Floats(u32, u32),
    Geo(f64, f64),
    Geos(u32, u32),
}

/// What the builder reads of each schema field: the fields a JSONL import type-checks.
pub(crate) struct Needed {
    /// Schema field index of each needed field, in schema order.
    pub fields: Vec<usize>,
    /// Needed-field slot of each schema field (NONE = not read).
    slot_of_field: Vec<u32>,
    /// Flattened key -> slot.
    by_key: FxHashMap<Box<str>, u32>,
}

impl Needed {
    pub fn new(schema: &Schema) -> Self {
        let mut fields = Vec::new();
        let mut slot_of_field = vec![NONE; schema.fields.len()];
        let mut by_key = FxHashMap::default();
        for (i, f) in schema.fields.iter().enumerate() {
            if f.field_type.is_dynamic_marker() {
                continue;
            }
            if !f.index && !f.facet && !f.sort && f.reference.is_none() {
                continue;
            }
            slot_of_field[i] = fields.len() as u32;
            by_key.insert(f.name.as_str().into(), fields.len() as u32);
            fields.push(i);
        }
        Self { fields, slot_of_field, by_key }
    }

    #[inline]
    pub fn slot(&self, field: usize) -> Option<usize> {
        let s = self.slot_of_field[field];
        (s != NONE).then_some(s as usize)
    }
}

/// Per registry id: the needed slot its flattened key names, and whether it is the top-level
/// `language` / `locale` key. Grows with the registry.
#[derive(Clone, Default)]
pub(crate) struct KeyMap {
    slot: Vec<u32>,
    path: Vec<Box<str>>,
    /// Key name of each entry (the last component of its path).
    name: Vec<Box<str>>,
    language: u32,
    locale: u32,
}

impl KeyMap {
    pub fn new() -> Self {
        Self { slot: Vec::new(), path: Vec::new(), name: Vec::new(), language: NONE, locale: NONE }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.slot.len()
    }

    /// Map the registry entries added since the last call.
    pub fn extend(&mut self, reg: &FieldRegistry, needed: &Needed, nested: bool) {
        for id in self.slot.len()..reg.len() {
            let e = reg.entry(id as u16);
            let path: Box<str> = if e.parent == NO_PARENT {
                e.name.clone()
            } else {
                format!("{}.{}", self.path[e.parent as usize], e.name).into()
            };
            // Without nested fields only top-level keys are looked up.
            let key_ok = nested || e.parent == NO_PARENT;
            self.slot.push(if key_ok { needed.by_key.get(&path).copied().unwrap_or(NONE) } else { NONE });
            if e.parent == NO_PARENT && &*e.name == "language" {
                self.language = id as u32;
            }
            if e.parent == NO_PARENT && &*e.name == "locale" {
                self.locale = id as u32;
            }
            self.path.push(path);
            self.name.push(e.name.clone());
        }
    }
}

#[derive(Clone, Copy)]
enum Slot {
    Unset,
    Direct { tag: u8, pos: u32 },
    /// An array pivoted from objects: `len` elements, the present ones in `pivot[start..end]`.
    Pivot { len: u32, start: u32, end: u32 },
}

#[derive(Clone, Copy)]
struct PivotVal {
    slot: u32,
    elem: u32,
    tag: u8,
    pos: u32,
}

/// One element of an array value: a tag and its payload position (NULL has no payload).
#[derive(Clone, Copy)]
struct Elem {
    tag: u8,
    pos: u32,
}

/// Scratch for one document at a time.
#[derive(Default)]
pub(crate) struct DocValues {
    slots: Vec<Slot>,
    touched: Vec<u32>,
    pivot: Vec<PivotVal>,
    elems: Vec<Elem>,
    pub typed: Vec<Typed>,
    pub strs: Vec<Option<(u32, u32)>>,
    pub ints: Vec<Option<i64>>,
    pub bools: Vec<Option<bool>>,
    pub floats: Vec<f32>,
    pub geos: Vec<(f64, f64)>,
    pub language: Language,
}

fn type_name(tag: u8) -> &'static str {
    match tag {
        TAG_NULL => "null",
        TAG_FALSE | TAG_TRUE | TAG_BOOL => "bool",
        TAG_INT | TAG_U64 | TAG_FLOAT => "number",
        TAG_STR => "string",
        TAG_ARRAY => "array",
        _ => "object",
    }
}

fn corrupt(e: RecordError) -> String {
    e.to_string()
}

/// Skip the payload of a value with tag `tag`.
fn skip(c: &mut Cursor<'_>, tag: u8) -> Result<(), RecordError> {
    match tag {
        TAG_INT | TAG_U64 => {
            c.varint()?;
        }
        TAG_FLOAT => {
            c.bytes(8)?;
        }
        TAG_STR => {
            let n = c.varint()? as usize;
            c.bytes(n)?;
        }
        TAG_FALSE | TAG_TRUE | TAG_NULL => {}
        TAG_BOOL => {
            c.byte()?;
        }
        TAG_OBJECT => {
            let n = c.varint()?;
            for _ in 0..n {
                let key = c.varint()?;
                skip(c, (key & 0xF) as u8)?;
            }
        }
        TAG_ARRAY => {
            let n = c.varint()?;
            if n > 0 {
                let t = c.byte()?;
                for _ in 0..n {
                    let et = if t == TAG_MIXED { c.byte()? } else { t };
                    skip(c, et)?;
                }
            }
        }
        _ => return Err(RecordError::Corrupt("unknown tag")),
    }
    Ok(())
}

impl DocValues {
    fn set(&mut self, slot: u32, v: Slot) {
        let s = slot as usize;
        if matches!(self.slots[s], Slot::Unset) {
            self.touched.push(slot);
        }
        self.slots[s] = v;
    }

    /// Read the typed values of `rec` (one record: id, then the top-level object). Err is the
    /// message the import would reject the document with.
    pub fn read(&mut self, rec: &[u8], schema: &Schema, needed: &Needed, keys: &KeyMap) -> Result<(), String> {
        if self.slots.len() != needed.fields.len() {
            self.slots = vec![Slot::Unset; needed.fields.len()];
        }
        for &s in &self.touched {
            self.slots[s as usize] = Slot::Unset;
        }
        self.touched.clear();
        self.pivot.clear();
        self.typed.clear();
        self.strs.clear();
        self.ints.clear();
        self.bools.clear();
        self.floats.clear();
        self.geos.clear();
        self.language = schema.default_language;

        let mut c = Cursor::new(rec);
        let id_len = c.varint().map_err(corrupt)? as usize;
        c.bytes(id_len).map_err(corrupt)?;
        let mut lang: Option<(u8, u32)> = None;
        let mut locale: Option<(u8, u32)> = None;
        self.walk_object(&mut c, rec, keys, schema.enable_nested_fields, true, &mut lang, &mut locale).map_err(corrupt)?;
        // `language`, else `locale`; a value that is not a known locale string is the collection's.
        if let Some((tag, pos)) = lang.or(locale) {
            if tag == TAG_STR {
                let s = str_at(rec, pos).map_err(corrupt)?;
                self.language = Language::from_locale(s).unwrap_or(schema.default_language);
            }
        }

        for (slot, &fi) in needed.fields.iter().enumerate() {
            let f = &schema.fields[fi];
            let t = match self.slots[slot] {
                Slot::Unset | Slot::Direct { tag: TAG_NULL, .. } => {
                    if !f.optional {
                        return Err(format!("Field `{}` has been declared in the schema, but is not found in the document.", f.name));
                    }
                    Typed::Absent
                }
                Slot::Direct { tag, pos } => {
                    self.typed_of(rec, keys, f, Value::Direct(tag, pos)).map_err(|e| format!("Field '{}': {}", f.name, e))?
                }
                Slot::Pivot { len, start, end } => {
                    self.typed_of(rec, keys, f, Value::Pivot(len, start, end)).map_err(|e| format!("Field '{}': {}", f.name, e))?
                }
            };
            self.typed.push(t);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_object(
        &mut self,
        c: &mut Cursor<'_>,
        rec: &[u8],
        keys: &KeyMap,
        nested: bool,
        top: bool,
        lang: &mut Option<(u8, u32)>,
        locale: &mut Option<(u8, u32)>,
    ) -> Result<(), RecordError> {
        let n = c.varint()?;
        for _ in 0..n {
            let key = c.varint()?;
            let (fid, tag) = ((key >> 4) as usize, (key & 0xF) as u8);
            if fid >= keys.len() {
                return Err(RecordError::Corrupt("field id outside registry"));
            }
            let pos = pos_of(c, rec);
            // `language` / `locale` as the flattened document has them: an object (or an array
            // of objects) is replaced by its dotted keys.
            if top && (fid as u32 == keys.language || fid as u32 == keys.locale) && !(nested && self.flattens(rec, tag, pos)?) {
                if fid as u32 == keys.language {
                    *lang = Some((tag, pos));
                } else {
                    *locale = Some((tag, pos));
                }
            }
            if nested && tag == TAG_OBJECT {
                self.walk_object(c, rec, keys, nested, false, lang, locale)?;
                continue;
            }
            if nested && tag == TAG_ARRAY {
                // Pivot when the first element is an object.
                let mut peek = Cursor::new(&rec[pos as usize..]);
                let len = peek.varint()?;
                if len > 0 {
                    let t = peek.byte()?;
                    let first = if t == TAG_MIXED { peek.byte()? } else { t };
                    if first == TAG_OBJECT {
                        let len_bytes = varint_len(len);
                        c.bytes(len_bytes + 1)?;
                        self.pivot_array(c, rec, keys, len as u32, t)?;
                        continue;
                    }
                }
            }
            let slot = keys.slot[fid];
            if slot != NONE {
                self.set(slot, Slot::Direct { tag, pos });
            }
            skip(c, tag)?;
        }
        Ok(())
    }

    /// Whether flattening replaces the value: an object, or an array whose first element is one.
    fn flattens(&self, rec: &[u8], tag: u8, pos: u32) -> Result<bool, RecordError> {
        if tag == TAG_OBJECT {
            return Ok(true);
        }
        if tag != TAG_ARRAY {
            return Ok(false);
        }
        let mut peek = Cursor::new(&rec[pos as usize..]);
        if peek.varint()? == 0 {
            return Ok(false);
        }
        let t = peek.byte()?;
        Ok((if t == TAG_MIXED { peek.byte()? } else { t }) == TAG_OBJECT)
    }

    /// The elements of an array whose first element is an object; the cursor is past the
    /// length and the element tag byte `t`.
    fn pivot_array(&mut self, c: &mut Cursor<'_>, rec: &[u8], keys: &KeyMap, len: u32, t: u8) -> Result<(), RecordError> {
        let from = self.pivot.len();
        for e in 0..len {
            let et = if t == TAG_MIXED { c.byte()? } else { t };
            if et != TAG_OBJECT {
                skip(c, et)?;
                continue;
            }
            let n = c.varint()?;
            for _ in 0..n {
                let key = c.varint()?;
                let (fid, tag) = ((key >> 4) as usize, (key & 0xF) as u8);
                if fid >= keys.len() {
                    return Err(RecordError::Corrupt("field id outside registry"));
                }
                let slot = keys.slot[fid];
                if slot != NONE {
                    self.pivot.push(PivotVal { slot, elem: e, tag, pos: pos_of(c, rec) });
                }
                skip(c, tag)?;
            }
        }
        if self.pivot.len() > from {
            self.pivot[from..].sort_by_key(|p| (p.slot, p.elem));
            let mut i = from;
            while i < self.pivot.len() {
                let slot = self.pivot[i].slot;
                let mut j = i;
                while j < self.pivot.len() && self.pivot[j].slot == slot {
                    j += 1;
                }
                self.set(slot, Slot::Pivot { len, start: i as u32, end: j as u32 });
                i = j;
            }
        }
        Ok(())
    }

    /// The array elements of `v` into `self.elems`; None when `v` is not an array.
    fn elements(&mut self, rec: &[u8], v: Value) -> Result<bool, String> {
        self.elems.clear();
        match v {
            Value::Direct(TAG_ARRAY, pos) => {
                let mut c = Cursor::new(rec);
                c.bytes(pos as usize).map_err(corrupt)?;
                let n = c.varint().map_err(corrupt)?;
                if n > 0 {
                    let t = c.byte().map_err(corrupt)?;
                    for _ in 0..n {
                        let et = if t == TAG_MIXED { c.byte().map_err(corrupt)? } else { t };
                        let p = pos_of(&c, rec);
                        self.elems.push(Elem { tag: et, pos: p });
                        skip(&mut c, et).map_err(corrupt)?;
                    }
                }
                Ok(true)
            }
            Value::Direct(..) => Ok(false),
            Value::Pivot(len, start, end) => {
                let mut k = start as usize;
                for e in 0..len {
                    if k < end as usize && self.pivot[k].elem == e {
                        let p = self.pivot[k];
                        self.elems.push(Elem { tag: p.tag, pos: p.pos });
                        k += 1;
                    } else {
                        self.elems.push(Elem { tag: TAG_NULL, pos: 0 });
                    }
                }
                Ok(true)
            }
        }
    }

    /// `json_to_field_value` on the record value.
    fn typed_of(&mut self, rec: &[u8], keys: &KeyMap, f: &FieldDef, v: Value) -> Result<Typed, String> {
        let scalar_name = |v: Value| match v {
            Value::Direct(tag, _) => type_name(tag),
            Value::Pivot(..) => "array",
        };
        let array_expected = |v: Value| format!("expected array, got {}", scalar_name(v));
        Ok(match &f.field_type {
            FieldType::Text | FieldType::String => match v {
                Value::Direct(TAG_STR, pos) => {
                    let (at, len) = str_range(rec, pos).map_err(corrupt)?;
                    Typed::Str(at, len)
                }
                _ => return Err(format!("expected string, got {}", scalar_name(v))),
            },
            FieldType::Int32 => match as_i64(rec, v).map_err(corrupt)? {
                Some(n) => Typed::Int(i64::from(n as i32)),
                None => return Err(format!("expected int32, got {}", scalar_name(v))),
            },
            FieldType::Int64 => match as_i64(rec, v).map_err(corrupt)? {
                Some(n) => Typed::Int(n),
                None => return Err(format!("expected int64, got {}", scalar_name(v))),
            },
            FieldType::Float64 => match as_f64(rec, v).map_err(corrupt)? {
                Some(x) => Typed::Float(x),
                None => return Err(format!("expected float, got {}", scalar_name(v))),
            },
            FieldType::Bool => match v {
                Value::Direct(TAG_TRUE, _) => Typed::Bool(true),
                Value::Direct(TAG_FALSE, _) => Typed::Bool(false),
                _ => return Err(format!("expected bool, got {}", scalar_name(v))),
            },
            FieldType::StringStar if matches!(v, Value::Direct(TAG_STR, _)) => {
                let Value::Direct(_, pos) = v else { unreachable!() };
                let start = self.strs.len() as u32;
                self.strs.push(Some(str_range(rec, pos).map_err(corrupt)?));
                Typed::Strs(start, start + 1)
            }
            FieldType::Object | FieldType::ObjectArray | FieldType::Auto => Typed::Absent,
            FieldType::StringArray | FieldType::StringStar => {
                if !self.elements(rec, v)? {
                    return Err(array_expected(v));
                }
                let start = self.strs.len() as u32;
                for i in 0..self.elems.len() {
                    let e = self.elems[i];
                    match e.tag {
                        TAG_NULL => self.strs.push(None),
                        TAG_STR => self.strs.push(Some(str_range(rec, e.pos).map_err(corrupt)?)),
                        _ => return Err("expected string or null in array".into()),
                    }
                }
                Typed::Strs(start, self.strs.len() as u32)
            }
            FieldType::BoolArray => {
                if !self.elements(rec, v)? {
                    return Err(array_expected(v));
                }
                let start = self.bools.len() as u32;
                for i in 0..self.elems.len() {
                    let e = self.elems[i];
                    let b = match e.tag {
                        TAG_NULL => None,
                        TAG_TRUE => Some(true),
                        TAG_FALSE => Some(false),
                        TAG_BOOL => Some(rec[e.pos as usize] != 0),
                        _ => return Err("expected bool or null in array".into()),
                    };
                    self.bools.push(b);
                }
                Typed::Bools(start, self.bools.len() as u32)
            }
            FieldType::Int32Array | FieldType::Int64Array => {
                if !self.elements(rec, v)? {
                    return Err(array_expected(v));
                }
                let int32 = f.field_type == FieldType::Int32Array;
                let start = self.ints.len() as u32;
                for i in 0..self.elems.len() {
                    let e = self.elems[i];
                    if e.tag == TAG_NULL {
                        self.ints.push(None);
                        continue;
                    }
                    match as_i64(rec, Value::Direct(e.tag, e.pos)).map_err(corrupt)? {
                        Some(n) => self.ints.push(Some(if int32 { i64::from(n as i32) } else { n })),
                        None => return Err("expected int or null in array".into()),
                    }
                }
                Typed::Ints(start, self.ints.len() as u32)
            }
            FieldType::GeoPointArray => {
                if !self.elements(rec, v)? {
                    return Err(array_expected(v));
                }
                let start = self.geos.len() as u32;
                let elems = std::mem::take(&mut self.elems);
                let r = (|| {
                    for e in &elems {
                        if e.tag == TAG_NULL {
                            continue;
                        }
                        let pair = (e.tag == TAG_ARRAY).then(|| self.pair(rec, e.pos)).transpose()?.flatten();
                        let Some((a, b)) = pair else { return Err("expected [lat, lng] pair in geopoint[]".to_string()) };
                        let lat = as_f64(rec, Value::Direct(a.tag, a.pos)).map_err(corrupt)?.ok_or("invalid lat")?;
                        let lng = as_f64(rec, Value::Direct(b.tag, b.pos)).map_err(corrupt)?.ok_or("invalid lng")?;
                        self.geos.push((lat, lng));
                    }
                    Ok(())
                })();
                self.elems = elems;
                r?;
                Typed::Geos(start, self.geos.len() as u32)
            }
            FieldType::GeoPoint => {
                if self.elements(rec, v)? && self.elems.len() == 2 {
                    let (a, b) = (self.elems[0], self.elems[1]);
                    let lat = as_f64(rec, Value::Direct(a.tag, a.pos)).map_err(corrupt)?.ok_or("expected float for lat")?;
                    let lng = as_f64(rec, Value::Direct(b.tag, b.pos)).map_err(corrupt)?.ok_or("expected float for lng")?;
                    return Ok(Typed::Geo(lat, lng));
                }
                if let Value::Direct(TAG_OBJECT, pos) = v {
                    let lat = object_number(rec, keys, pos, "lat").map_err(corrupt)?.ok_or("expected 'lat' float in geopoint")?;
                    let lng = object_number(rec, keys, pos, "lng").map_err(corrupt)?.ok_or("expected 'lng' float in geopoint")?;
                    return Ok(Typed::Geo(lat, lng));
                }
                return Err(format!("expected [lat, lng] or {{lat, lng}}, got {}", scalar_name(v)));
            }
            FieldType::FloatArray { num_dim } => {
                if !self.elements(rec, v)? {
                    return Err(array_expected(v));
                }
                let start = self.floats.len() as u32;
                for i in 0..self.elems.len() {
                    let e = self.elems[i];
                    if *num_dim == 0 && e.tag == TAG_NULL {
                        continue;
                    }
                    match as_f64(rec, Value::Direct(e.tag, e.pos)).map_err(corrupt)? {
                        Some(x) => self.floats.push(x as f32),
                        None => return Err("expected float in array".into()),
                    }
                }
                Typed::Floats(start, self.floats.len() as u32)
            }
        })
    }

    /// The two elements of the array at `pos`, when it has exactly two.
    fn pair(&self, rec: &[u8], pos: u32) -> Result<Option<(Elem, Elem)>, String> {
        let mut c = Cursor::new(rec);
        c.bytes(pos as usize).map_err(corrupt)?;
        let n = c.varint().map_err(corrupt)?;
        if n != 2 {
            return Ok(None);
        }
        let t = c.byte().map_err(corrupt)?;
        let mut out = [Elem { tag: 0, pos: 0 }; 2];
        for o in &mut out {
            let et = if t == TAG_MIXED { c.byte().map_err(corrupt)? } else { t };
            *o = Elem { tag: et, pos: pos_of(&c, rec) };
            skip(&mut c, et).map_err(corrupt)?;
        }
        Ok(Some((out[0], out[1])))
    }
}

/// `obj.get(name).and_then(as_f64)` for the object at `pos` (a geopoint given as an object, which
/// stays an object only without nested fields).
fn object_number(rec: &[u8], keys: &KeyMap, pos: u32, name: &str) -> Result<Option<f64>, RecordError> {
    let mut c = Cursor::new(rec);
    c.bytes(pos as usize)?;
    let n = c.varint()?;
    for _ in 0..n {
        let key = c.varint()?;
        let (fid, tag) = ((key >> 4) as usize, (key & 0xF) as u8);
        if fid >= keys.len() {
            return Err(RecordError::Corrupt("field id outside registry"));
        }
        if &*keys.name[fid] == name {
            return as_f64(rec, Value::Direct(tag, pos_of(&c, rec)));
        }
        skip(&mut c, tag)?;
    }
    Ok(None)
}

#[derive(Clone, Copy)]
enum Value {
    Direct(u8, u32),
    Pivot(u32, u32, u32),
}

#[inline]
fn pos_of(c: &Cursor<'_>, rec: &[u8]) -> u32 {
    (rec.len() - c.remaining()) as u32
}

#[inline]
fn varint_len(mut v: u64) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

fn str_range(rec: &[u8], pos: u32) -> Result<(u32, u32), RecordError> {
    let mut c = Cursor::new(&rec[pos as usize..]);
    let n = c.varint()? as usize;
    let at = pos as usize + (rec.len() - pos as usize - c.remaining());
    if at + n > rec.len() {
        return Err(RecordError::Corrupt("string past end"));
    }
    std::str::from_utf8(&rec[at..at + n]).map_err(|_| RecordError::Corrupt("invalid UTF-8"))?;
    Ok((at as u32, n as u32))
}

fn str_at(rec: &[u8], pos: u32) -> Result<&str, RecordError> {
    let (at, n) = str_range(rec, pos)?;
    // SAFETY: str_range checked the bytes are UTF-8.
    Ok(unsafe { std::str::from_utf8_unchecked(&rec[at as usize..(at + n) as usize]) })
}

/// The string of a `Typed::Str` or of a `strs` entry.
#[inline]
pub(crate) fn str_of(rec: &[u8], (at, len): (u32, u32)) -> &str {
    // SAFETY: ranges come from str_range, which checked the bytes are UTF-8.
    unsafe { std::str::from_utf8_unchecked(&rec[at as usize..(at + len) as usize]) }
}

fn as_i64(rec: &[u8], v: Value) -> Result<Option<i64>, RecordError> {
    let Value::Direct(tag, pos) = v else { return Ok(None) };
    let mut c = Cursor::new(&rec[pos as usize..]);
    Ok(match tag {
        TAG_INT => {
            let z = c.varint()?;
            Some(((z >> 1) as i64) ^ -((z & 1) as i64))
        }
        // U64 records hold only values above i64::MAX.
        _ => None,
    })
}

fn as_f64(rec: &[u8], v: Value) -> Result<Option<f64>, RecordError> {
    let Value::Direct(tag, pos) = v else { return Ok(None) };
    let mut c = Cursor::new(&rec[pos as usize..]);
    Ok(match tag {
        TAG_INT => {
            let z = c.varint()?;
            Some((((z >> 1) as i64) ^ -((z & 1) as i64)) as f64)
        }
        TAG_U64 => Some(c.varint()? as f64),
        TAG_FLOAT => Some(c.f64()?),
        _ => None,
    })
}
