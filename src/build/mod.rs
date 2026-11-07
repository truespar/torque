//! Streaming segment builder (TQBF spec §10.2).
//!
//! Documents arrive as TQBF records (spec §5.2) encoded against the builder's registry, for
//! example by the .NET client straight from its objects. They come in runs: a run is a stretch
//! of consecutive doc ids, added on one thread (`RunBuilder`) and indexed as its records arrive
//! into compact accumulators (interned terms and values, (term, doc, tf) triples, ordinals,
//! zstd-compressed record blocks). A run freezes into sorted, delta-coded pieces whenever its
//! accumulators reach `piece_bytes`, and at its end; frozen pieces go to a spill file, so the
//! builder's memory does not grow with the collection. `finish` maps the spill file, merges the
//! pieces field by field, in parallel, and streams the segment file.
//!
//! The index sections equal those of a JSONL import of the same documents byte for byte (the
//! same encoders write them). No document is ever decoded into a JSON value; records are read in
//! place and stored as they are.
//!
//! Runs are laid out in key order: the caller numbers its runs (for example by the CompanyId
//! range they cover), so the doc id order is fixed by the data, not by thread timing. With keys
//! 0, 1, 2, ... the stored documents' dictionary is trained on the first 8 MB of records in key
//! order as soon as those runs have them, and blocks are compressed while records arrive; with
//! other keys it is trained at `finish` on the first records in doc id order. Either way the file
//! depends only on the input. Document ids must be unique within a build.

mod doc;
mod intern;
mod merge;
mod run;

pub use run::RunBuilder;

use crate::index::values::{self, Ords};
use crate::schema::{FieldType, Schema};
use crate::tqbf::registry::FieldRegistry;
use doc::{KeyMap, Needed};
use run::FrozenRun;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Records the stored documents' zstd dictionary is trained on.
pub(crate) const MAX_SAMPLE_BYTES: usize = 8 * 1024 * 1024;
const DICT_SIZE: usize = 16 * 1024;
/// A run freezes a piece when its accumulators hold about this much.
pub(crate) const PIECE_BYTES: usize = 64 << 20;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// zstd level of the stored documents.
    pub docs_level: i32,
    /// Threads `finish` merges with (0 = the global rayon pool).
    pub threads: usize,
    /// A run freezes a piece when its accumulators hold about this many bytes.
    pub piece_bytes: usize,
    /// Directory of the spill file frozen pieces go to (None: they stay in memory).
    pub spill_dir: Option<PathBuf>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self { docs_level: 19, threads: 0, piece_bytes: PIECE_BYTES, spill_dir: Some(std::env::temp_dir()) }
    }
}

/// A document the builder refused, with the message a JSONL import gives.
#[derive(Clone, Debug)]
pub struct Rejected {
    pub run: u64,
    /// Position among the records added to the run.
    pub index: u64,
    pub id: String,
    pub error: String,
}

#[derive(Debug)]
pub struct BuildReport {
    pub docs: u32,
    pub pieces: usize,
    pub rejected: Vec<Rejected>,
    /// File size.
    pub bytes: u64,
    /// Bytes of the frozen pieces (spilled or held) before the merge.
    pub frozen_bytes: usize,
    pub merge_millis: u128,
}

// -- plan ----------------------------------------------------------------------------------------

pub(crate) struct TextPlan {
    pub field: usize,
    pub slot: usize,
}

pub(crate) struct ValuePlan {
    pub field: usize,
    pub slot: usize,
    pub strings: bool,
    pub ords: Ords,
    pub float: bool,
}

pub(crate) struct BoolPlan {
    pub field: usize,
    pub slot: usize,
    pub sort: bool,
}

pub(crate) struct GeoPlan {
    pub field: usize,
    pub slot: usize,
    pub multi: bool,
}

/// What every schema field gets, as `build_index` decides it.
#[derive(Default)]
pub(crate) struct BuildPlan {
    pub text: Vec<TextPlan>,
    pub values: Vec<ValuePlan>,
    pub bools: Vec<BoolPlan>,
    pub geos: Vec<GeoPlan>,
}

impl BuildPlan {
    fn new(schema: &Schema, needed: &Needed) -> io::Result<Self> {
        let mut p = Self::default();
        for (i, f) in schema.fields.iter().enumerate() {
            if f.field_type.is_dynamic_marker() {
                return Err(invalid(format!("field `{}`: dynamic fields (object, object[], auto) need a server-side import", f.name)));
            }
            if f.embed.is_some() {
                return Err(invalid(format!("field `{}`: auto-embedding fields need a server-side import", f.name)));
            }
            if matches!(f.field_type, FieldType::FloatArray { num_dim } if num_dim > 0) {
                return Err(invalid(format!("field `{}`: vector fields are not built here yet", f.name)));
            }
            let Some(slot) = needed.slot(i) else { continue };
            if f.index && f.field_type.is_searchable() {
                p.text.push(TextPlan { field: i, slot });
            }
            match values::plan(f) {
                Some(values::Plan::Strings(ords)) => p.values.push(ValuePlan { field: i, slot, strings: true, ords, float: false }),
                Some(values::Plan::Numbers { ords, float }) => p.values.push(ValuePlan { field: i, slot, strings: false, ords, float }),
                Some(values::Plan::Bools) => p.bools.push(BoolPlan { field: i, slot, sort: f.sort }),
                None => {}
            }
            match f.field_type {
                FieldType::GeoPoint => p.geos.push(GeoPlan { field: i, slot, multi: false }),
                FieldType::GeoPointArray => p.geos.push(GeoPlan { field: i, slot, multi: true }),
                _ => {}
            }
        }
        Ok(p)
    }
}

// -- shared state --------------------------------------------------------------------------------

pub(crate) struct Shared {
    pub schema: Schema,
    pub schema_json: Vec<u8>,
    pub needed: Needed,
    pub plan: BuildPlan,
    pub opts: BuildOptions,
    /// Registry id -> needed slot, republished when paths are added.
    pub keys: arc_swap::ArcSwap<KeyMap>,
    registry: Mutex<FieldRegistry>,
    registry_len: AtomicUsize,
    dict: OnceLock<Arc<Vec<u8>>>,
    samples: Mutex<Samples>,
    pieces: Mutex<Vec<FrozenRun>>,
    spill: Mutex<Option<Spill>>,
    rejected: Mutex<Vec<Rejected>>,
    frozen_bytes: AtomicUsize,
}

/// Runs' first records, gathered in key order until they make the dictionary sample.
#[derive(Default)]
struct Samples {
    pending: BTreeMap<u64, Vec<Vec<u8>>>,
    next: u64,
    taken: Vec<Vec<u8>>,
    bytes: usize,
}

/// The file frozen pieces are written to; mapped by `finish`, removed with the builder.
pub(crate) struct Spill {
    path: PathBuf,
    file: io::BufWriter<std::fs::File>,
    pos: u64,
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Some(sp) = self.spill.get_mut().ok().and_then(Option::take) {
            let path = sp.path.clone();
            drop(sp);
            let _ = std::fs::remove_file(path);
        }
    }
}

fn train(sample: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = sample.iter().map(Vec::len).sum();
    if sample.len() < 64 || total < DICT_SIZE * 32 {
        return Vec::new();
    }
    zstd::dict::from_samples(sample, DICT_SIZE).unwrap_or_default()
}

impl Shared {
    #[inline]
    pub fn registry_len(&self) -> usize {
        self.registry_len.load(Ordering::Acquire)
    }

    /// The stored documents' dictionary once trained (empty = compress without one).
    pub fn dictionary(&self) -> Option<Arc<Vec<u8>>> {
        self.dict.get().cloned()
    }

    pub fn train_dictionary(&self, sample: &[Vec<u8>]) {
        self.dict.get_or_init(|| Arc::new(train(sample)));
    }

    /// Run `key`'s first records (all of them, or at least `MAX_SAMPLE_BYTES`). The dictionary
    /// is trained once runs 0, 1, 2, ... in key order give `MAX_SAMPLE_BYTES` of records.
    pub fn submit_sample(&self, key: u64, sample: Vec<Vec<u8>>) {
        if self.dict.get().is_some() {
            return;
        }
        let mut s = self.samples.lock().unwrap();
        s.pending.insert(key, sample);
        loop {
            let next = s.next;
            let Some(recs) = s.pending.remove(&next) else { return };
            for r in recs {
                if s.bytes >= MAX_SAMPLE_BYTES {
                    break;
                }
                s.bytes += r.len();
                s.taken.push(r);
            }
            s.next += 1;
            if s.bytes >= MAX_SAMPLE_BYTES {
                let sample = std::mem::take(&mut s.taken);
                s.pending.clear();
                drop(s);
                self.train_dictionary(&sample);
                return;
            }
        }
    }

    /// Keep a frozen piece: in the spill file when there is one.
    pub fn add_piece(&self, mut piece: FrozenRun) -> io::Result<()> {
        use std::io::Write;
        static ZEROS: [u8; 8] = [0; 8];
        self.frozen_bytes.fetch_add(piece.bytes(), Ordering::Relaxed);
        if let Some(dir) = &self.opts.spill_dir {
            let mut g = self.spill.lock().unwrap();
            if g.is_none() {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let path = dir.join(format!("torque-build-{}-{}.spill", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
                let file = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
                *g = Some(Spill { path, file: io::BufWriter::with_capacity(4 << 20, file), pos: 0 });
            }
            let sp = g.as_mut().unwrap();
            let mut spots = Vec::new();
            piece.visit(&mut |mut col| {
                let len = col.bytes().len() as u64;
                let at = sp.pos.next_multiple_of(8);
                sp.file.write_all(&ZEROS[..(at - sp.pos) as usize])?;
                sp.file.write_all(col.bytes())?;
                spots.push((at, len));
                sp.pos = at + len;
                col.set(crate::tqbf::bytes::Bytes::from_vec(Vec::new()))
            })?;
            piece.spilled = Some(spots);
        }
        self.pieces.lock().unwrap().push(piece);
        Ok(())
    }

    /// Map the spill file and point every spilled piece at it (once all runs have ended).
    pub fn load_spilled(&self, pieces: &mut [FrozenRun]) -> io::Result<()> {
        use std::io::Write;
        let mut g = self.spill.lock().unwrap();
        let Some(sp) = g.as_mut() else { return Ok(()) };
        sp.file.flush()?;
        // SAFETY: the spill file is only appended to by this builder, and every run has ended.
        let map = unsafe { memmap2::Mmap::map(sp.file.get_ref())? };
        let bytes = crate::tqbf::bytes::Bytes::from_map(Arc::new(map));
        for p in pieces {
            let Some(spots) = p.spilled.take() else { continue };
            let mut i = 0;
            p.visit(&mut |mut col| {
                let (at, len) = spots[i];
                i += 1;
                col.set(bytes.slice(at as usize, len as usize))
            })?;
        }
        Ok(())
    }

    pub fn add_rejected(&self, rejected: &[Rejected]) {
        self.rejected.lock().unwrap().extend_from_slice(rejected);
    }

    fn publish(&self, reg: &FieldRegistry) {
        if reg.len() > self.keys.load().len() {
            let mut keys = (**self.keys.load()).clone();
            keys.extend(reg, &self.needed, self.schema.enable_nested_fields);
            self.keys.store(Arc::new(keys));
        }
        self.registry_len.store(reg.len(), Ordering::Release);
    }
}

// -- builder -------------------------------------------------------------------------------------

/// Builds one segment file from runs of records; see the module documentation.
pub struct SegmentBuilder {
    shared: Arc<Shared>,
}

impl SegmentBuilder {
    /// `schema_json` is the collection schema as `POST /collections` takes it.
    pub fn new(schema_json: &[u8], opts: BuildOptions) -> io::Result<Self> {
        Self::create(schema_json, opts, None)
    }

    /// A builder for records already encoded against `registry` (a docs-only file's FIELDS):
    /// the builder's registry is that one with the schema's paths added, so the records are
    /// indexed and stored as they are.
    pub fn with_registry(schema_json: &[u8], opts: BuildOptions, registry: &FieldRegistry) -> io::Result<Self> {
        Self::create(schema_json, opts, Some(registry))
    }

    fn create(schema_json: &[u8], opts: BuildOptions, records_registry: Option<&FieldRegistry>) -> io::Result<Self> {
        let mut schema: Schema = serde_json::from_slice(schema_json).map_err(|e| invalid(format!("schema: {e}")))?;
        crate::schema::validate_schema(&schema).map_err(|e| invalid(format!("schema: {}", e.message)))?;
        schema.assign_field_ids();
        let needed = Needed::new(&schema);
        let plan = BuildPlan::new(&schema, &needed)?;
        let registry = match records_registry {
            Some(r) => {
                let mut r = r.clone();
                r.add_schema(&schema);
                r
            }
            None => (*schema.registry.load()).clone(),
        };
        let mut keys = KeyMap::new();
        keys.extend(&registry, &needed, schema.enable_nested_fields);
        let schema_json = serde_json::to_vec(&schema).map_err(|e| invalid(e.to_string()))?;
        let registry_len = AtomicUsize::new(registry.len());
        Ok(Self {
            shared: Arc::new(Shared {
                schema,
                schema_json,
                needed,
                plan,
                opts,
                keys: arc_swap::ArcSwap::from_pointee(keys),
                registry: Mutex::new(registry),
                registry_len,
                dict: OnceLock::new(),
                samples: Mutex::new(Samples::default()),
                pieces: Mutex::new(Vec::new()),
                spill: Mutex::new(None),
                rejected: Mutex::new(Vec::new()),
                frozen_bytes: AtomicUsize::new(0),
            }),
        })
    }

    /// Registry id of key `name` under `parent` (`registry::NO_PARENT` at the top level),
    /// added when new. Records must only use ids handed out here (or by `registry`).
    pub fn path_id(&self, parent: u16, name: &str) -> io::Result<u16> {
        let mut reg = self.shared.registry.lock().unwrap();
        if parent != crate::tqbf::registry::NO_PARENT && parent as usize >= reg.len() {
            return Err(invalid(format!("parent id {parent} is not in the registry")));
        }
        let id = reg.ensure(parent, name).map_err(|e| invalid(e.to_string()))?;
        self.shared.publish(&reg);
        Ok(id)
    }

    /// The registry as it stands (the schema's paths come first).
    pub fn registry(&self) -> FieldRegistry {
        self.shared.registry.lock().unwrap().clone()
    }

    /// Encode a JSON document as a record against the builder's registry (tests and tools; a
    /// client encodes its own records).
    pub fn encode_json(&self, doc: &serde_json::Value) -> io::Result<Vec<u8>> {
        use crate::tqbf::record;
        let id = record::document_id(doc).ok_or_else(|| invalid("document has no string `id`"))?;
        let mut reg = self.shared.registry.lock().unwrap();
        let rec = record::encode(doc, id, &mut reg).map_err(|e| invalid(e.to_string()))?;
        self.shared.publish(&reg);
        Ok(rec)
    }

    /// Start run `key`. Keys must be unique; runs are laid out in key order.
    pub fn run(&self, key: u64) -> RunBuilder {
        RunBuilder::new(Arc::clone(&self.shared), key)
    }

    /// Merge every ended run and write the segment file to `path`.
    pub fn finish(self, path: &Path) -> io::Result<BuildReport> {
        merge::finish(&self.shared, path)
    }
}

#[cfg(test)]
mod tests;
