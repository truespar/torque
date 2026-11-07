//! FIELDS section: every JSON path that occurs in a collection's documents gets a stable id.
//! Schema fields carry their schema flags; paths that only occur in documents are `STORED_ONLY`
//! (returned in hits, never indexed or type-checked). Ids are append-only for the life of a
//! collection, so records written with an older registry stay readable with a newer one.

use crate::schema::{FieldType, Schema};
use rustc_hash::FxHashMap;

pub const NO_PARENT: u16 = u16::MAX;

pub mod flags {
    pub const IN_SCHEMA: u16 = 1 << 0;
    pub const INDEX: u16 = 1 << 1;
    pub const FACET: u16 = 1 << 2;
    pub const SORT: u16 = 1 << 3;
    pub const OPTIONAL: u16 = 1 << 4;
    pub const STORED_ONLY: u16 = 1 << 5;
    pub const INFIX: u16 = 1 << 6;
    pub const STEM: u16 = 1 << 7;
    pub const REFERENCE: u16 = 1 << 8;
}

/// Typesense type names in code order; code 0 = not a schema field.
const TYPE_NAMES: [&str; 17] = [
    "", "string", "int32", "int64", "float", "bool", "string[]", "int32[]", "int64[]", "geopoint",
    "float[]", "string*", "bool[]", "geopoint[]", "object", "object[]", "auto",
];

fn type_code(t: &FieldType) -> u8 {
    TYPE_NAMES.iter().position(|n| *n == t.as_str()).unwrap_or(0) as u8
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldEntry {
    pub parent: u16,
    pub flags: u16,
    pub value_type: u8,
    pub name: Box<str>,
}

impl FieldEntry {
    pub fn type_name(&self) -> &'static str {
        TYPE_NAMES[self.value_type as usize]
    }
}

#[derive(Clone, Debug, Default)]
pub struct FieldRegistry {
    entries: Vec<FieldEntry>,
    /// children[parent + 1] maps a key name to its id; index 0 is the top level.
    children: Vec<FxHashMap<Box<str>, u16>>,
}

#[derive(Debug)]
pub struct RegistryFull;

impl std::fmt::Display for RegistryFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "collection has more than {} distinct document paths", u16::MAX - 1)
    }
}

impl FieldRegistry {
    pub fn new() -> Self {
        Self { entries: Vec::new(), children: vec![FxHashMap::default()] }
    }

    /// Registry holding the schema's fields (and their enclosing object paths).
    pub fn from_schema(schema: &Schema) -> Self {
        let mut reg = Self::new();
        reg.add_schema(schema);
        reg
    }

    /// Register schema fields not yet present and refresh flags of those that are.
    pub fn add_schema(&mut self, schema: &Schema) {
        for def in &schema.fields {
            if def.field_type.is_dynamic_marker() {
                continue;
            }
            let mut parent = NO_PARENT;
            let parts: Vec<&str> = if schema.enable_nested_fields {
                def.name.split('.').collect()
            } else {
                vec![def.name.as_str()]
            };
            for (i, part) in parts.iter().enumerate() {
                let Ok(id) = self.ensure(parent, part) else { return };
                if i + 1 == parts.len() {
                    let e = &mut self.entries[id as usize];
                    let mut f = flags::IN_SCHEMA;
                    if def.index { f |= flags::INDEX; }
                    if def.facet { f |= flags::FACET; }
                    if def.sort { f |= flags::SORT; }
                    if def.optional { f |= flags::OPTIONAL; }
                    if def.stem { f |= flags::STEM; }
                    if def.reference.is_some() { f |= flags::REFERENCE; }
                    e.flags = f;
                    e.value_type = type_code(&def.field_type);
                }
                parent = id;
            }
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn entry(&self, id: u16) -> &FieldEntry {
        &self.entries[id as usize]
    }

    #[inline]
    pub fn lookup(&self, parent: u16, name: &str) -> Option<u16> {
        self.children[parent.wrapping_add(1) as usize].get(name).copied()
    }

    /// Id of `name` under `parent`, adding a stored-only entry when the path is new.
    pub fn ensure(&mut self, parent: u16, name: &str) -> Result<u16, RegistryFull> {
        if let Some(id) = self.lookup(parent, name) {
            return Ok(id);
        }
        if self.entries.len() >= (u16::MAX - 1) as usize {
            return Err(RegistryFull);
        }
        let id = self.entries.len() as u16;
        self.entries.push(FieldEntry { parent, flags: flags::STORED_ONLY, value_type: 0, name: name.into() });
        self.children.push(FxHashMap::default());
        self.children[parent.wrapping_add(1) as usize].insert(name.into(), id);
        Ok(id)
    }

    /// Full dotted path of an entry.
    pub fn path(&self, id: u16) -> String {
        let mut parts = Vec::new();
        let mut cur = id;
        while cur != NO_PARENT {
            let e = &self.entries[cur as usize];
            parts.push(&*e.name);
            cur = e.parent;
        }
        parts.reverse();
        parts.join(".")
    }

    /// Ids of a dotted path, or None when any component is unknown.
    pub fn resolve_path(&self, path: &str) -> Option<u16> {
        let mut parent = NO_PARENT;
        for part in path.split('.') {
            parent = self.lookup(parent, part)?;
        }
        Some(parent)
    }

    /// Take the paths a log record carries (`persistence::wal::Entry::Writes`), each under its id:
    /// those held already must be the same, the rest come in order after them.
    pub fn extend_with(&mut self, paths: &[(u16, FieldEntry)]) -> Result<(), String> {
        for (id, p) in paths {
            let i = *id as usize;
            if i < self.entries.len() {
                let have = &self.entries[i];
                if have.parent != p.parent || have.name != p.name {
                    return Err(format!("path {i} is `{}` in the registry, `{}` in the log", self.path(*id), p.name));
                }
                continue;
            }
            if i != self.entries.len() {
                return Err(format!("the log's paths skip from {} to {i}", self.entries.len()));
            }
            if self.lookup(p.parent, &p.name).is_some() || (p.parent != NO_PARENT && p.parent as usize >= i) {
                return Err(format!("the log's path {i} (`{}`) is held under another id", p.name));
            }
            self.ensure(p.parent, &p.name).map_err(|e| e.to_string())?;
            self.entries[i].flags = p.flags;
            self.entries[i].value_type = p.value_type;
        }
        Ok(())
    }

    /// True when `self` holds every entry of `older` under the same id (append-only growth).
    pub fn extends(&self, older: &FieldRegistry) -> bool {
        older.entries.len() <= self.entries.len()
            && older
                .entries
                .iter()
                .zip(&self.entries)
                .all(|(a, b)| a.parent == b.parent && a.name == b.name)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.entries.len() * 16);
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for e in &self.entries {
            out.extend_from_slice(&e.parent.to_le_bytes());
            out.extend_from_slice(&e.flags.to_le_bytes());
            out.push(e.value_type);
            out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            out.extend_from_slice(e.name.as_bytes());
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let err = || "FIELDS section truncated".to_string();
        let mut pos = 0usize;
        let take = |pos: &mut usize, n: usize| -> Result<&[u8], String> {
            let s = bytes.get(*pos..*pos + n).ok_or_else(err)?;
            *pos += n;
            Ok(s)
        };
        let count = u32::from_le_bytes(take(&mut pos, 4)?.try_into().unwrap()) as usize;
        let mut reg = Self::new();
        for i in 0..count {
            let parent = u16::from_le_bytes(take(&mut pos, 2)?.try_into().unwrap());
            let fl = u16::from_le_bytes(take(&mut pos, 2)?.try_into().unwrap());
            let value_type = take(&mut pos, 1)?[0];
            let len = u16::from_le_bytes(take(&mut pos, 2)?.try_into().unwrap()) as usize;
            let name = std::str::from_utf8(take(&mut pos, len)?).map_err(|e| e.to_string())?;
            if parent != NO_PARENT && parent as usize >= i {
                return Err(format!("FIELDS entry {i} has forward parent {parent}"));
            }
            if (value_type as usize) >= TYPE_NAMES.len() {
                return Err(format!("FIELDS entry {i} has unknown type code {value_type}"));
            }
            let id = reg.ensure(parent, name).map_err(|e| e.to_string())?;
            if id as usize != i {
                return Err(format!("FIELDS entry {i} duplicates entry {id}"));
            }
            let e = &mut reg.entries[i];
            e.flags = fl;
            e.value_type = value_type;
        }
        Ok(reg)
    }
}

/// The collection's registry, shared by every clone of its `Schema`. Reads are lock-free; a
/// document with a path never seen before clones, extends and republishes it under a lock.
#[derive(Clone)]
pub struct SharedRegistry(std::sync::Arc<SharedInner>);

struct SharedInner {
    current: arc_swap::ArcSwap<FieldRegistry>,
    write: std::sync::Mutex<()>,
    /// Set once a record has been encoded against the registry; until then a registry restored
    /// from disk may replace it.
    used: std::sync::atomic::AtomicBool,
}

impl Default for SharedRegistry {
    fn default() -> Self {
        Self(std::sync::Arc::new(SharedInner {
            current: arc_swap::ArcSwap::from_pointee(FieldRegistry::new()),
            write: std::sync::Mutex::new(()),
            used: std::sync::atomic::AtomicBool::new(false),
        }))
    }
}

impl std::fmt::Debug for SharedRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SharedRegistry({} paths)", self.load().len())
    }
}

impl SharedRegistry {
    #[inline]
    pub fn load(&self) -> std::sync::Arc<FieldRegistry> {
        self.0.current.load_full()
    }

    pub fn add_schema(&self, schema: &Schema) {
        let _w = self.0.write.lock().unwrap();
        let mut next = (*self.0.current.load_full()).clone();
        next.add_schema(schema);
        self.0.current.store(std::sync::Arc::new(next));
    }

    /// Take over a registry read from a persisted segment. Returns false when the two registries
    /// assign different ids to the same position and records were already encoded locally.
    pub fn adopt(&self, stored: &FieldRegistry, schema: &Schema) -> bool {
        use std::sync::atomic::Ordering;
        let _w = self.0.write.lock().unwrap();
        let cur = self.0.current.load_full();
        if cur.extends(stored) {
            return true;
        }
        if !stored.extends(&cur) && self.0.used.load(Ordering::Acquire) {
            return false;
        }
        let mut next = stored.clone();
        next.add_schema(schema);
        self.0.current.store(std::sync::Arc::new(next));
        true
    }

    /// Take the paths a log record carries (`FieldRegistry::extend_with`).
    pub fn extend_with(&self, paths: &[(u16, FieldEntry)]) -> Result<(), String> {
        if paths.iter().all(|(id, _)| (*id as usize) < self.0.current.load().len()) {
            let cur = self.0.current.load();
            return paths.iter().try_for_each(|(id, p)| {
                let have = cur.entry(*id);
                (have.parent == p.parent && have.name == p.name).then_some(()).ok_or_else(|| format!("path {id} is `{}` in the registry, `{}` in the log", cur.path(*id), p.name))
            });
        }
        let _w = self.0.write.lock().unwrap();
        let mut next = (*self.0.current.load_full()).clone();
        next.extend_with(paths)?;
        self.0.current.store(std::sync::Arc::new(next));
        Ok(())
    }

    /// Encode a document, registering new paths when it has any.
    pub fn encode(&self, doc: &serde_json::Value, id: &str) -> Result<Vec<u8>, super::record::RecordError> {
        use super::record::{encode, encode_known, RecordError};
        use std::sync::atomic::Ordering;
        self.0.used.store(true, Ordering::Release);
        match encode_known(doc, id, &self.0.current.load()) {
            Err(RecordError::UnknownPath) => {
                let _w = self.0.write.lock().unwrap();
                let mut next = (*self.0.current.load_full()).clone();
                let rec = encode(doc, id, &mut next)?;
                self.0.current.store(std::sync::Arc::new(next));
                Ok(rec)
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{FieldDef, Schema};

    fn schema() -> Schema {
        Schema {
            name: "c".into(),
            enable_nested_fields: true,
            fields: vec![
                FieldDef { name: "names.nameOrIdentifier".into(), field_type: FieldType::StringArray, ..Default::default() },
                FieldDef { name: "companyId".into(), field_type: FieldType::Int32, ..Default::default() },
                FieldDef { name: "names.firstSeenAt".into(), field_type: FieldType::Int64Array, index: false, optional: true, ..Default::default() },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn schema_paths_and_round_trip() {
        let mut reg = FieldRegistry::from_schema(&schema());
        let names = reg.resolve_path("names").unwrap();
        let leaf = reg.resolve_path("names.nameOrIdentifier").unwrap();
        assert_eq!(reg.entry(leaf).parent, names);
        assert!(reg.entry(names).flags & flags::STORED_ONLY != 0);
        assert!(reg.entry(leaf).flags & flags::INDEX != 0);
        let fs = reg.resolve_path("names.firstSeenAt").unwrap();
        assert_eq!(reg.entry(fs).flags & flags::INDEX, 0);
        assert_eq!(reg.entry(fs).type_name(), "int64[]");
        let extra = reg.ensure(names, "nameType").unwrap();
        assert_eq!(reg.path(extra), "names.nameType");

        let back = FieldRegistry::decode(&reg.encode()).unwrap();
        assert_eq!(back.len(), reg.len());
        assert!(back.extends(&reg) && reg.extends(&back));
        for id in 0..reg.len() as u16 {
            assert_eq!(back.entry(id), reg.entry(id));
        }
    }
}
