pub mod disk;
pub mod index;
pub mod wal;

use crate::auth::PersistedKey;
use crate::collection::alias::AliasInfo;
use crate::collection::{Collection, CollectionError, CollectionRegistry};
use crate::index::IndexBundle;
use crate::presets::Preset;
use crate::schema::Schema;
use crate::stopwords::StopwordSet;
use crate::synonyms::SynonymSet;
use arc_swap::ArcSwap;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;

/// Persisted form of a collection (schema + metadata, no index data).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedCollection {
    #[serde(flatten)]
    pub schema: Schema,
    pub created_at: u64,
    /// `Collection::schema_version` (0 in a file written before versions).
    #[serde(default)]
    pub schema_version: u64,
    /// Suspended: its files kept, not loaded until resumed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suspended: bool,
}

impl PersistedCollection {
    fn of(col: &Collection) -> Self {
        PersistedCollection { schema: (*col.schema).clone(), created_at: col.created_at, schema_version: col.schema_version, suspended: false }
    }
}

fn save_json<T: Serialize>(data_dir: &Path, filename: &str, data: &[T]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let json = serde_json::to_string_pretty(data)
        .map_err(|e| std::io::Error::other(e))?;
    let path = data_dir.join(filename);
    // A whole new file renamed over the old one: a crash leaves the one or the other, never a
    // part. The name is the save's own, so two saves never write one file.
    let tmp = data_dir.join(format!("{filename}.{}.tmp", COUNTER.fetch_add(1, Ordering::Relaxed)));
    let mut f = disk::create(&tmp)?;
    f.write_all(json.as_bytes())?;
    disk::sync(&f, &tmp)?;
    drop(f);
    disk::rename(&tmp, &path)?;
    disk::sync_parent(&path)
}

/// Remove the temporary files of `save_json`s a crash cut short (the file each would have replaced
/// is whole), as the server starts: before anything saves.
fn remove_temporaries(data_dir: &Path) {
    for e in std::fs::read_dir(data_dir).into_iter().flatten().flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".tmp")
            && name.contains(".json.")
            && e.path().is_file()
            && let Err(err) = disk::remove_file(&e.path())
        {
            tracing::warn!(file = %e.path().display(), error = %err, "A temporary file of a save cut short is not removed");
        }
    }
}

fn load_json<T: DeserializeOwned>(data_dir: &Path, filename: &str) -> std::io::Result<Vec<T>> {
    let path = data_dir.join(filename);
    if !path.exists() {
        return Ok(vec![]);
    }
    let contents = std::fs::read_to_string(&path)?;
    serde_json::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

// -- Collections persistence (custom logic, not generic) --
//
// collections.json lists every collection: those loaded, and those not (suspended, or not loadable
// from their files). A collection's files are its segment set (indexes/{name}) and its log
// (wal/{name}); files of a name collections.json does not list are a deletion's a crash cut short,
// removed at the next start.
//
// A save of collections.json that fails may have replaced the file or not (its rename done, the
// sync after failing). The change it would commit is then neither done nor undone: the registry
// takes the side that acknowledges nothing more (a collection created or installed is not there, a
// deleted one stays deleted, a suspended one suspended), and no file goes on that ground. Whichever
// file a restart finds, nothing acknowledged is lost: the collection it lists has its files, and
// those of one it does not are removed then.

/// Save collections.json: the collections loaded and those not. One save at a time, each of the
/// registry as it is then, so the last one on disk holds every change saved.
pub fn save_collections(data_dir: &Path, registry: &CollectionRegistry) -> std::io::Result<()> {
    static SAVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _saving = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    let mut collections: Vec<PersistedCollection> = registry.list().iter().map(|c| PersistedCollection::of(c)).collect();
    collections.extend(registry.unloaded());
    collections.sort_by(|a, b| a.schema.name.cmp(&b.schema.name));
    save_json(data_dir, "collections.json", &collections)
}

/// Create a collection (`CollectionRegistry::create`) and save collections.json: once it returns,
/// the collection is there after a restart. When collections.json does not take it the collection
/// is not there either, and the error says so.
pub fn create_collection(data_dir: &Path, registry: &CollectionRegistry, schema: Schema) -> Result<Arc<Collection>, CollectionError> {
    let collection = registry.create(schema)?;
    if let Err(e) = save_collections(data_dir, registry) {
        let _ = registry.delete(&collection.name);
        collection.close_wal();
        return Err(CollectionError::Storage(format!("Could not save the collection: {e}")));
    }
    Ok(collection)
}

/// Delete a collection: out of the registry and collections.json, the deletion's commit. Its files
/// go after (`remove_collection_files`). When collections.json does not take the deletion the
/// collection stays deleted, its files kept (a restart that finds it listed has it whole), and the
/// error says so.
pub fn delete_collection(data_dir: &Path, registry: &CollectionRegistry, name: &str) -> Result<Arc<Collection>, CollectionError> {
    let deleted = registry.delete(name)?;
    if let Err(e) = save_collections(data_dir, registry) {
        deleted.close_wal();
        return Err(CollectionError::Storage(format!("Could not save the deletion: {e}")));
    }
    Ok(deleted)
}

/// Delete a collection set aside at the start (`load_collections`: its files could not be read, as
/// those of another format version are): out of collections.json, the deletion's commit, then its
/// files, which nothing maps. What a crash leaves of them the next start removes. Its entry; NotFound
/// when none of the name is set aside (a suspended one is kept on purpose: resumed first). When
/// collections.json does not take the deletion it stays set aside, and the error says so.
pub fn delete_set_aside(data_dir: &Path, registry: &CollectionRegistry, name: &str) -> Result<PersistedCollection, CollectionError> {
    let not_found = || CollectionError::NotFound(name.to_string());
    if !registry.unloaded().iter().any(|e| e.schema.name == name && !e.suspended) {
        return Err(not_found());
    }
    let entry = registry.take_unloaded(name).ok_or_else(not_found)?;
    if let Err(e) = save_collections(data_dir, registry) {
        registry.set_unloaded(entry);
        return Err(CollectionError::Storage(format!("Could not save the deletion: {e}")));
    }
    if let Err(e) = index::delete_index(data_dir, name) {
        tracing::error!(collection = name, "Failed to delete index files: {e}");
    }
    if let Err(e) = wal::remove(data_dir, name) {
        tracing::error!(collection = name, "Failed to delete the write-ahead log: {e}");
    }
    Ok(entry)
}

/// Remove a deleted collection's files: its segment set (once the searches holding them let go of
/// its mapped files, on Windows) and its log. What a crash leaves of them the next start removes.
pub fn remove_collection_files(data_dir: &Path, deleted: &Collection) {
    deleted.index.store(Arc::new(IndexBundle::empty(Arc::clone(&deleted.schema))));
    let mut attempt = 0u32;
    loop {
        match index::delete_index(data_dir, &deleted.name) {
            Ok(()) => break,
            // (a file a search still maps: access denied, a sharing violation, a mapped file)
            Err(e) if cfg!(windows) && attempt < 100 && (e.kind() == std::io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(32 | 1224))) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                tracing::error!(collection = deleted.name.as_str(), "Failed to delete index files: {e}");
                break;
            }
        }
    }
    deleted.close_wal();
    if let Err(e) = wal::remove(data_dir, &deleted.name) {
        tracing::error!(collection = deleted.name.as_str(), "Failed to delete the write-ahead log: {e}");
    }
}

/// Suspend a collection: saved, its log closed with every write in it on disk, kept in
/// collections.json as suspended and out of the registry. The collection, with its document
/// count.
pub fn suspend_collection(data_dir: &Path, registry: &CollectionRegistry, name: &str) -> Result<u32, CollectionError> {
    let collection = registry.get(name).ok_or_else(|| CollectionError::NotFound(name.to_string()))?;
    if !collection.schema.persistence_enabled {
        return Err(CollectionError::InvalidSchema(format!("Collection `{name}` has persistence disabled. Cannot suspend without data loss.")));
    }
    collection.save(data_dir).map_err(|e| CollectionError::Storage(format!("Failed to persist index before suspend: {e}")))?;
    // its log closes with every write on disk: those after the save are replayed at resume
    collection.close_wal();
    let docs = collection.snapshot_bundle().total_docs();
    let mut entry = PersistedCollection::of(&collection);
    entry.suspended = true;
    registry.delete(name)?;
    registry.set_unloaded(entry);
    if let Err(e) = save_collections(data_dir, registry) {
        // (suspended still: saved and closed, whichever collections.json a restart finds)
        return Err(CollectionError::Storage(format!("Failed to update collections.json: {e}")));
    }
    Ok(docs)
}

/// Load a collection not loaded (suspended, or not loadable at the start) and save collections.json.
pub fn resume_collection(data_dir: &Path, registry: &CollectionRegistry, name: &str, metrics: Option<&crate::observability::MetricsCollector>) -> Result<Arc<Collection>, CollectionError> {
    if registry.get(name).is_some() {
        return Err(CollectionError::AlreadyExists(name.to_string()));
    }
    let entry = registry.take_unloaded(name).ok_or_else(|| CollectionError::NotFound(name.to_string()))?;
    let (entry, _) = committed(data_dir, name, entry);
    let collection = match open_collection(data_dir, &entry, metrics) {
        Ok(c) => c,
        Err(e) => {
            registry.set_unloaded(entry);
            return Err(CollectionError::Storage(format!("Failed to load index for `{name}`: {e}")));
        }
    };
    registry.insert(Arc::clone(&collection));
    if let Err(e) = save_collections(data_dir, registry) {
        // suspended still after a restart: set aside again
        let _ = registry.delete(name);
        collection.close_wal();
        registry.set_unloaded(entry);
        return Err(CollectionError::Storage(format!("Failed to update collections.json: {e}")));
    }
    Ok(collection)
}

/// Create a collection of segment files (an upload's): moved into its segment set
/// (`index::install_segments`), then collections.json saved. On an error the collection is not
/// there: its files go at once when collections.json was not reached, else as `create_collection`.
pub fn install_collection(data_dir: &Path, registry: &CollectionRegistry, schema: Schema, files: &[std::path::PathBuf]) -> Result<Arc<Collection>, CollectionError> {
    let collection = registry.create(schema)?;
    let bundle = match index::install_segments(data_dir, &collection.name, files, &collection.schema, collection.stamp()) {
        Ok(bundle) => bundle,
        Err(e) => {
            let _ = registry.delete(&collection.name);
            remove_collection_files(data_dir, &collection);
            return Err(CollectionError::Storage(format!("Could not install the segments: {e}")));
        }
    };
    collection.swap_bundle(bundle);
    if let Err(e) = save_collections(data_dir, registry) {
        let _ = registry.delete(&collection.name);
        return Err(CollectionError::Storage(format!("Could not save the collection: {e}")));
    }
    Ok(collection)
}

/// The collection as last committed: `entry` (collections.json's), or the schema its bundle's
/// manifest commits when that one is newer: a schema change whose collections.json save a crash
/// cut short (`CollectionRegistry::change_schema`). True when it is the manifest's.
fn committed(data_dir: &Path, name: &str, entry: PersistedCollection) -> (PersistedCollection, bool) {
    match index::committed_schema(data_dir, name) {
        Some((schema, stamp)) if stamp.created_at == entry.created_at && stamp.version > entry.schema_version => {
            tracing::warn!(
                collection = name,
                from = entry.schema_version,
                to = stamp.version,
                "Finishing a schema change a restart cut short: the schema its segments were saved under"
            );
            (PersistedCollection { schema, created_at: stamp.created_at, schema_version: stamp.version, suspended: entry.suspended }, true)
        }
        _ => (entry, false),
    }
}

/// The collection of this name as collections.json holds it (`committed`), loaded or not.
pub fn load_persisted(data_dir: &Path, name: &str) -> std::io::Result<Option<PersistedCollection>> {
    let persisted: Vec<PersistedCollection> = load_json(data_dir, "collections.json")?;
    Ok(persisted.into_iter().find(|p| p.schema.name == name).map(|e| committed(data_dir, name, e).0))
}

/// Open a collection of collections.json: its saved segments, mapped (the files of saves a crash
/// cut short removed: nothing writes them yet), and for a realtime one with persistence the writes
/// its log holds after them. An error when its segment set cannot be read.
fn open_collection(data_dir: &Path, entry: &PersistedCollection, metrics: Option<&crate::observability::MetricsCollector>) -> Result<Arc<Collection>, String> {
    let mut schema = entry.schema.clone();
    schema.assign_field_ids();
    let name = schema.name.clone();
    let schema = Arc::new(schema);
    let start = std::time::Instant::now();
    let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| index::load_bundle(data_dir, &name, &schema, entry.created_at)));
    let bundle = match loaded {
        Ok(Ok(Some(bundle))) => {
            index::remove_strays(data_dir, &name);
            tracing::info!(collection = name.as_str(), docs = bundle.total_docs(), segments = bundle.segment_count(), "Restored index bundle from disk");
            if let Some(m) = metrics {
                m.persistence_load_duration
                    .get_or_create(&crate::observability::CollectionLabels { collection: name.clone() })
                    .observe(start.elapsed().as_secs_f64());
            }
            bundle
        }
        Ok(Ok(None)) => IndexBundle::empty(Arc::clone(&schema)),
        Ok(Err(e)) => return Err(e.to_string()),
        Err(_) => return Err("its segment set could not be read (a panic reading it)".into()),
    };
    let bundle = Arc::new(bundle);
    // A realtime collection's writes after its saved segments: its write-ahead log's records
    let (bundle, wal) = if schema.index_mode == crate::schema::IndexMode::Realtime && schema.persistence_enabled {
        crate::collection::recover(data_dir, &name, &schema, entry.schema_version, bundle)
    } else {
        (bundle, None)
    };
    // every path is the saved segment set's manifest's or the log's
    let logged_paths = schema.registry.load().len();
    Ok(Arc::new(Collection {
        name,
        schema,
        index: ArcSwap::new(bundle),
        created_at: entry.created_at,
        schema_version: entry.schema_version,
        last_rebuilt_at: std::sync::atomic::AtomicU64::new(0),
        last_compaction_at: std::sync::atomic::AtomicU64::new(0),
        last_save_at: std::sync::atomic::AtomicU64::new(0),
        generation: std::sync::atomic::AtomicU64::new(0),
        wal: std::sync::Mutex::new(wal),
        logged_paths: std::sync::atomic::AtomicUsize::new(logged_paths),
        data_dir: Some(data_dir.to_path_buf()),
        compacting: std::sync::atomic::AtomicBool::new(false),
        flush_waiting: std::sync::atomic::AtomicBool::new(false),
        maintaining: std::sync::atomic::AtomicBool::new(false),
        writes: std::sync::Mutex::new(()),
        queue: Default::default(),
        successor: Default::default(),
        saves: Default::default(),
    }))
}

/// Load the collections of collections.json, as the server starts: each one loaded, or kept
/// unloaded when suspended or when its files cannot be read (an error logged; its files kept, the
/// next start or a resume tries again). Then the files of names collections.json does not list
/// (deletions a crash cut short) are removed. An error when collections.json cannot be read: the
/// server does not start then rather than lose the collections it lists.
pub fn load_collections(
    data_dir: &Path,
    registry: &CollectionRegistry,
    metrics: Option<&crate::observability::MetricsCollector>,
) -> std::io::Result<()> {
    remove_temporaries(data_dir);
    let listed = data_dir.join("collections.json").is_file();
    let persisted: Vec<PersistedCollection> = load_json(data_dir, "collections.json")
        .map_err(|e| std::io::Error::new(e.kind(), format!("collections.json: {e}")))?;
    let mut finished = false;

    for entry in persisted {
        let name = entry.schema.name.clone();
        let (entry, from_manifest) = committed(data_dir, &name, entry);
        finished |= from_manifest;
        if entry.suspended {
            registry.set_unloaded(entry);
            continue;
        }
        match open_collection(data_dir, &entry, metrics) {
            Ok(collection) => registry.insert(collection),
            Err(e) => {
                tracing::error!(
                    collection = name.as_str(),
                    error = %e,
                    "The collection could not be loaded: it is set aside with its files, and loaded at the next start or by resume"
                );
                registry.set_unloaded(entry);
            }
        }
    }

    // the schema changes finished here, in collections.json too
    if finished && let Err(e) = save_collections(data_dir, registry) {
        tracing::error!("Failed to persist collections after finishing a schema change: {e}");
    }
    if listed {
        remove_unlisted(data_dir, registry);
    }
    Ok(())
}

/// Remove the files of collections collections.json does not list: deletions a crash cut short.
fn remove_unlisted(data_dir: &Path, registry: &CollectionRegistry) {
    let mut known: rustc_hash::FxHashSet<String> = registry.list().iter().map(|c| c.name.clone()).collect();
    known.extend(registry.unloaded().into_iter().map(|e| e.schema.name));
    for (dir, is_log) in [(data_dir.join("indexes"), false), (data_dir.join("wal"), true)] {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if known.contains(&name) || !e.path().is_dir() {
                continue;
            }
            let removed = if is_log { wal::remove(data_dir, &name) } else { index::delete_index(data_dir, &name) };
            match removed {
                Ok(()) => tracing::info!(collection = name.as_str(), "Removed the files of a deleted collection"),
                Err(err) => tracing::warn!(collection = name.as_str(), error = %err, "The files of a deleted collection are not removed"),
            }
        }
    }
}

// -- Generic save/load for aliases, synonyms, presets, stopwords, keys --

pub fn save_aliases(data_dir: &Path, aliases: &[AliasInfo]) -> std::io::Result<()> {
    save_json(data_dir, "aliases.json", aliases)
}

pub fn load_aliases(data_dir: &Path) -> std::io::Result<Vec<AliasInfo>> {
    load_json(data_dir, "aliases.json")
}

pub fn save_synonyms(data_dir: &Path, sets: &[SynonymSet]) -> std::io::Result<()> {
    save_json(data_dir, "synonyms.json", sets)
}

pub fn save_stemming_dictionaries(
    data_dir: &Path,
    dicts: &[crate::stemming::StemmingDictionary],
) -> std::io::Result<()> {
    save_json(data_dir, "stemming_dictionaries.json", dicts)
}

pub fn load_stemming_dictionaries(
    data_dir: &Path,
) -> std::io::Result<Vec<crate::stemming::StemmingDictionary>> {
    load_json(data_dir, "stemming_dictionaries.json")
}

pub fn load_synonyms(data_dir: &Path) -> std::io::Result<Vec<SynonymSet>> {
    load_json(data_dir, "synonyms.json")
}

pub fn save_curations(
    data_dir: &Path,
    sets: &[crate::curation::CurationSet],
) -> std::io::Result<()> {
    save_json(data_dir, "curation_sets.json", sets)
}

pub fn load_curations(data_dir: &Path) -> std::io::Result<Vec<crate::curation::CurationSet>> {
    load_json(data_dir, "curation_sets.json")
}

pub fn save_analytics_rules(
    data_dir: &Path,
    rules: &[crate::analytics::AnalyticsRule],
) -> std::io::Result<()> {
    save_json(data_dir, "analytics_rules.json", rules)
}

pub fn load_analytics_rules(
    data_dir: &Path,
) -> std::io::Result<Vec<crate::analytics::AnalyticsRule>> {
    load_json(data_dir, "analytics_rules.json")
}

pub fn save_conversation_models(
    data_dir: &Path,
    models: &[crate::llm::ConversationModel],
) -> std::io::Result<()> {
    save_json(data_dir, "conversation_models.json", models)
}

pub fn load_conversation_models(
    data_dir: &Path,
) -> std::io::Result<Vec<crate::llm::ConversationModel>> {
    load_json(data_dir, "conversation_models.json")
}

pub fn save_nl_search_models(
    data_dir: &Path,
    models: &[crate::llm::NlSearchModel],
) -> std::io::Result<()> {
    save_json(data_dir, "nl_search_models.json", models)
}

pub fn load_nl_search_models(
    data_dir: &Path,
) -> std::io::Result<Vec<crate::llm::NlSearchModel>> {
    load_json(data_dir, "nl_search_models.json")
}

pub fn save_presets(data_dir: &Path, presets: &[Preset]) -> std::io::Result<()> {
    save_json(data_dir, "presets.json", presets)
}

pub fn load_presets(data_dir: &Path) -> std::io::Result<Vec<Preset>> {
    load_json(data_dir, "presets.json")
}

pub fn save_stopwords(data_dir: &Path, sets: &[StopwordSet]) -> std::io::Result<()> {
    save_json(data_dir, "stopwords.json", sets)
}

pub fn load_stopwords(data_dir: &Path) -> std::io::Result<Vec<StopwordSet>> {
    load_json(data_dir, "stopwords.json")
}

pub fn save_keys(data_dir: &Path, keys: &[PersistedKey]) -> std::io::Result<()> {
    save_json(data_dir, "keys.json", keys)
}

pub fn load_keys(data_dir: &Path) -> std::io::Result<Vec<PersistedKey>> {
    load_json(data_dir, "keys.json")
}
