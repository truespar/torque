//! Crashes at every change a collection makes to its data directory
//! (`persistence::disk::fault`). A scenario runs once to count its changes, then once for each,
//! the disk stopping there. Each state that crash could leave on disk (the process stopping; a
//! power loss under POSIX's rules and NTFS's, with each part of the journal kept; the data written
//! after a sync torn) is put on disk and recovered. The collection loads, holds every acknowledged
//! step and the one under way whole or not at all, answers searches as one segment of those
//! documents does, keeps no file its manifest does not hold, and takes writes again; a deleted one
//! leaves no file. A crash in that recovery recovers too.
//!
//! Each scenario also runs once for each of its changes failing (an I/O error, the disk going on):
//! after a restart every acknowledged step holds, and anything else is what a failed step did.

use super::*;
use crate::persistence::disk::fault::{CrashState, Disk, Model, Plan};
use crate::schema::{FieldDef, FieldType, FieldValue, IndexMode};
use crate::search::{SearchParams, SearchTarget};
use std::path::{Path, PathBuf};

const NAME: &str = "crash";

fn field(name: &str, field_type: FieldType) -> FieldDef {
    FieldDef { name: name.into(), field_type, ..Default::default() }
}

fn schema() -> Schema {
    let mut n = field("n", FieldType::Int32);
    n.facet = true;
    n.sort = true;
    Schema {
        name: NAME.into(),
        fields: vec![field("title", FieldType::String), n],
        index_mode: IndexMode::Realtime,
        persistence_enabled: true,
        ..Default::default()
    }
}

fn doc(schema: &Schema, id: &str, title: &str, n: i64) -> Document {
    crate::ingest::parse_json_document(&serde_json::json!({"id": id, "title": title, "n": n}), schema, 0).unwrap()
}

/// (id, title, n)
type Doc = (String, String, i64);

/// A collection as the acknowledged steps made it.
#[derive(Clone, Debug, Default, PartialEq)]
struct State {
    exists: bool,
    suspended: bool,
    fields: Vec<String>,
    /// In `_seq_id` order.
    docs: Vec<Doc>,
}

impl State {
    fn new(docs: &[(&str, &str, i64)]) -> State {
        let mut s = State { exists: true, suspended: false, fields: vec!["title".into(), "n".into()], docs: Vec::new() };
        s.write(docs, &[]);
        s
    }

    fn write(&mut self, docs: &[(&str, &str, i64)], deletes: &[&str]) {
        for &(id, title, n) in docs {
            let d = (id.to_string(), title.to_string(), n);
            match self.docs.iter_mut().find(|x| x.0 == id) {
                Some(x) => *x = d,
                None => self.docs.push(d),
            }
        }
        self.docs.retain(|x| !deletes.contains(&x.0.as_str()));
    }

    fn of(col: &Collection) -> State {
        State { exists: true, suspended: false, fields: col.schema.fields.iter().map(|f| f.name.clone()).collect(), docs: contents(col) }
    }
}

fn contents(col: &Collection) -> Vec<Doc> {
    let (title, n) = (col.schema.name_to_id["title"] as usize, col.schema.name_to_id["n"] as usize);
    col.live_documents_as(&col.schema, false)
        .unwrap()
        .into_iter()
        .map(|d| {
            let title = match d.fields.get(title) {
                Some(FieldValue::Text(t)) => t.clone(),
                other => panic!("title {other:?}"),
            };
            let n = match d.fields.get(n) {
                Some(FieldValue::Int32(n)) => i64::from(*n),
                other => panic!("n {other:?}"),
            };
            (d.external_id, title, n)
        })
        .collect()
}

/// Wait until the collection's log has taken what was sent to it (a save's checkpoint).
fn settle(col: &Collection) {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(col.wal_barrier());
}

/// What one value of a collection is: a document of it, or the collection's own state.
#[derive(Clone, Debug, PartialEq)]
enum Key {
    Exists,
    Suspended,
    Fields,
    Doc(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Flag(bool),
    Fields(Vec<String>),
    Doc(Option<(String, i64)>),
}

impl Key {
    fn of(&self, s: &State) -> Value {
        match self {
            Key::Exists => Value::Flag(s.exists),
            Key::Suspended => Value::Flag(s.suspended),
            Key::Fields => Value::Fields(s.fields.clone()),
            Key::Doc(id) => Value::Doc(s.docs.iter().find(|d| d.0 == *id).map(|d| (d.1.clone(), d.2))),
        }
    }
}

/// What a step sets, value by value, made from `state` (a document written again with what it
/// held is set all the same: told apart from documents it leaves by their title).
fn sets(effect: &impl Fn(&mut State), state: &State) -> Vec<(Key, Value)> {
    const LEFT: &str = "\u{1}";
    let mut after = state.clone();
    effect(&mut after);
    let mut out: Vec<(Key, Value)> = [Key::Exists, Key::Suspended, Key::Fields].into_iter().filter(|k| k.of(&after) != k.of(state)).map(|k| {
        let v = k.of(&after);
        (k, v)
    }).collect();
    let mut probe = state.clone();
    for d in &mut probe.docs {
        d.1 = LEFT.into();
    }
    effect(&mut probe);
    let ids = state.docs.iter().chain(&probe.docs).map(|d| d.0.clone());
    let mut seen = Vec::new();
    for id in ids {
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());
        let key = Key::Doc(id);
        match key.of(&probe) {
            Value::Doc(Some((title, _))) if title == LEFT => {}
            Value::Doc(None) if key.of(state) == Value::Doc(None) => {}
            v => out.push((key, v)),
        }
    }
    out
}

/// A scenario under way: its collection, and what its steps acknowledged.
struct Run<'a> {
    dir: PathBuf,
    disk: &'a Disk,
    reg: Arc<CollectionRegistry>,
    metrics: crate::observability::MetricsCollector,
    acked: State,
    /// The state had the step the disk stopped in taken effect.
    in_flight: Option<State>,
    stopped: bool,
    /// Merges the steps made, flushes' aside.
    merges: usize,
    /// A change fails and the disk goes on (`Plan::Fail`): steps go on after one fails.
    failing: bool,
    /// Every step taken effect, failed or not.
    shadow: State,
    /// Each step's values set, and whether it was acknowledged.
    history: Vec<(Vec<(Key, Value)>, bool)>,
}

impl<'a> Run<'a> {
    fn new(dir: &Path, disk: &'a Disk, failing: bool) -> Self {
        Run {
            dir: dir.to_path_buf(),
            disk,
            reg: Arc::new(CollectionRegistry::new(Some(dir.to_path_buf()))),
            metrics: crate::observability::MetricsCollector::new(),
            acked: State::default(),
            in_flight: None,
            stopped: false,
            merges: 0,
            failing,
            shadow: State::default(),
            history: Vec::new(),
        }
    }

    fn col(&self) -> Result<Arc<Collection>, String> {
        self.reg.get(NAME).ok_or_else(|| "no collection".to_string())
    }

    /// A step: `make` makes it, `effect` is what it does to the collection once acknowledged. A
    /// step fails only because the disk stopped, and none is made after; or, failing, because a
    /// change failed, and the steps go on.
    fn step(&mut self, effect: impl Fn(&mut State), make: impl FnOnce(&mut Self) -> Result<(), String>) {
        if self.stopped {
            return;
        }
        if self.failing {
            let set = sets(&effect, &self.shadow);
            effect(&mut self.shadow);
            let made = make(self);
            self.history.push((set, made.is_ok()));
            return;
        }
        let mut next = self.acked.clone();
        effect(&mut next);
        let made = make(self);
        let stopped = self.disk.stopped();
        match made {
            Ok(()) => self.acked = next,
            Err(e) => {
                assert!(stopped.is_some(), "a step failed, the disk going: {e}");
                if next != self.acked {
                    self.in_flight = Some(next);
                }
            }
        }
        self.stopped = stopped.is_some();
    }

    fn create(&mut self) {
        self.step(|s| *s = State::new(&[]), |run| crate::persistence::create_collection(&run.dir, &run.reg, schema()).map(drop).map_err(|e| e.to_string()));
    }

    /// Upsert `docs` then delete `deletes`, as one write, and wait for its acknowledgement.
    fn write(&mut self, docs: &[(&str, &str, i64)], deletes: &[&str]) {
        self.step(
            |s| s.write(docs, deletes),
            |run| {
                let col = run.col()?;
                let docs = docs.iter().map(|&(id, title, n)| doc(&col.schema, id, title, n)).collect();
                let deletes: Vec<String> = deletes.iter().map(|d| d.to_string()).collect();
                col.apply_writes(docs, &deletes).map_err(|e| e.to_string())?.expect("a logged collection").wait_blocking()
            },
        );
    }

    fn truncate(&mut self) {
        self.step(|s| s.docs.clear(), |run| run.col()?.truncate().map_err(|e| e.to_string())?.expect("a logged collection").wait_blocking());
    }

    /// A flush as the server makes one: the segments in memory into one, the flushed ones merged
    /// as due, saved.
    fn flush(&mut self) {
        self.step(
            |_| {},
            |run| {
                let col = run.col()?;
                compaction::flush(&col, Some(&run.dir), &run.metrics);
                settle(&col);
                Ok(())
            },
        );
    }

    fn merge_memory(&mut self) {
        self.step(
            |_| {},
            |run| {
                run.merges += run.col()?.merge_memory();
                Ok(())
            },
        );
    }

    /// Merge the flushed segments due (a segment with many deleted copies alone), and save.
    fn reclaim(&mut self) {
        self.step(
            |_| {},
            |run| {
                let col = run.col()?;
                run.merges += compaction::reclaim(&col, Some(&run.dir), &run.metrics);
                settle(&col);
                Ok(())
            },
        );
    }

    fn save(&mut self) {
        self.step(
            |_| {},
            |run| {
                let col = run.col()?;
                col.save(&run.dir).map_err(|e| e.to_string())?;
                settle(&col);
                Ok(())
            },
        );
    }

    /// A schema change adding an optional field: the collection made again from its documents.
    fn add_field(&mut self) {
        self.step(
            |s| s.fields.push("extra".into()),
            |run| {
                let mut extra = field("extra", FieldType::String);
                extra.optional = true;
                let new = run.reg.change_schema(NAME, vec![extra], None, None, None, crate::ingest::build_index).map_err(|e| e.to_string())?;
                settle(&new);
                Ok(())
            },
        );
    }

    /// The server shut down (its logs closed) and started again.
    fn restart(&mut self) {
        self.step(
            |_| {},
            |run| {
                for c in run.reg.list() {
                    c.close_wal();
                }
                run.reg = load(&run.dir)?;
                Ok(())
            },
        );
    }

    /// Delete the collection, as the server does: out of collections.json, then its files.
    fn delete(&mut self) {
        self.step(
            |s| *s = State::default(),
            |run| {
                let deleted = crate::persistence::delete_collection(&run.dir, &run.reg, NAME).map_err(|e| e.to_string())?;
                crate::persistence::remove_collection_files(&run.dir, &deleted);
                Ok(())
            },
        );
    }

    fn suspend(&mut self) {
        self.step(|s| s.suspended = true, |run| crate::persistence::suspend_collection(&run.dir, &run.reg, NAME).map(drop).map_err(|e| e.to_string()));
    }

    fn resume(&mut self) {
        self.step(|s| s.suspended = false, |run| crate::persistence::resume_collection(&run.dir, &run.reg, NAME, None).map(drop).map_err(|e| e.to_string()));
    }

    /// An upload, as the server takes one: a segment file of `docs` staged, installed as a new
    /// batch collection, the staging directory removed.
    fn install(&mut self, docs: &[(&str, &str, i64)]) {
        self.step(
            |s| *s = State::new(docs),
            |run| {
                let mut batch = schema();
                batch.index_mode = IndexMode::Batch;
                let mut typed = batch.clone();
                typed.assign_field_ids();
                let staging = run.dir.join("uploads").join(NAME);
                crate::persistence::disk::create_dirs(&staging).map_err(|e| e.to_string())?;
                let index = crate::ingest::build_index(&typed, docs.iter().map(|&(id, title, n)| doc(&typed, id, title, n)).collect());
                let part = staging.join("part_00000000.tqbf");
                let json = serde_json::to_vec(&typed).unwrap();
                crate::persistence::index::write_segment(&index, Some(&json), &part).map_err(|e| e.to_string())?;
                crate::persistence::install_collection(&run.dir, &run.reg, batch, &[part]).map_err(|e| e.to_string())?;
                crate::persistence::disk::remove_dir_all(&staging).ok();
                Ok(())
            },
        );
    }

    /// The process is gone: what was acknowledged, and what was under way.
    fn end(self) -> (State, Option<State>) {
        (self.acked, self.in_flight)
    }
}

/// The collections of `dir` loaded, as a server starting.
fn load(dir: &Path) -> Result<Arc<CollectionRegistry>, String> {
    crate::persistence::index::forget_under(dir);
    let reg = Arc::new(CollectionRegistry::new(Some(dir.to_path_buf())));
    crate::persistence::load_collections(dir, &reg, None).map_err(|e| format!("loading: {e}"))?;
    Ok(reg)
}

/// The states a crash where `disk` stopped could leave, each once: under POSIX's rules, and on
/// Windows NTFS's too (the code there syncs files where Unix syncs directories).
fn crash_states(disk: &Disk, seed: u64) -> Vec<CrashState> {
    let models: &[Model] = if cfg!(windows) { &[Model::Posix, Model::Ntfs] } else { &[Model::Posix] };
    let mut states: Vec<CrashState> = Vec::new();
    for st in models.iter().flat_map(|&m| disk.crash_states(m, seed)) {
        if !states.iter().any(|s| s.same(&st)) {
            states.push(st);
        }
    }
    states
}

/// What a scenario did, run without a crash.
struct Done {
    merges: usize,
    segments: usize,
}

/// Run `scenario` with the disk stopping at each of its changes in turn, and recover every state
/// each crash could leave (crashing those recoveries too). On one thread, so a scenario makes its
/// changes in one order.
fn crash_everywhere(scenario: fn(&mut Run)) -> Done {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    pool.install(|| {
        let (total, done) = {
            let dir = tempfile::tempdir().unwrap();
            let disk = Disk::arm(dir.path(), Plan::Watch);
            let mut run = Run::new(dir.path(), &disk, false);
            scenario(&mut run);
            assert!(!run.stopped);
            let segments = run.reg.get(NAME).map_or(0, |c| c.snapshot_bundle().segments.len());
            let done = Done { merges: run.merges, segments };
            drop(run);
            (disk.changes(), done)
        };
        let (mut states_all, mut recoveries) = (0, 0);
        for k in 1..=total {
            let dir = tempfile::tempdir().unwrap();
            let disk = Disk::arm(dir.path(), Plan::Crash(k));
            let mut run = Run::new(dir.path(), &disk, false);
            scenario(&mut run);
            let at = disk.stopped().unwrap_or_else(|| panic!("change {k} of {total}: the disk did not stop"));
            let (acked, in_flight) = run.end();
            let states = crash_states(&disk, k);
            drop(disk);
            states_all += states.len();
            for st in &states {
                recoveries += recover_crashing(dir.path(), st, &acked, in_flight.as_ref()).unwrap_or_else(|e| panic!("a crash at change {k} of {total} ({at}), {}: {e}", st.label));
            }
        }
        if std::env::var_os("TORQUE_CRASH_STATS").is_some() {
            eprintln!("{total} changes, {states_all} states, {recoveries} recoveries");
        }
        done
    })
}

/// Run `scenario` with each of its changes failing in turn (an I/O error, the disk going on), shut
/// down and start again: every acknowledged step holds, the rest is what failed steps did, and the
/// collection is whole.
fn fail_everywhere(scenario: fn(&mut Run)) {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    pool.install(|| {
        let total = {
            let dir = tempfile::tempdir().unwrap();
            let disk = Disk::arm(dir.path(), Plan::Watch);
            let mut run = Run::new(dir.path(), &disk, true);
            scenario(&mut run);
            assert!(run.history.iter().all(|h| h.1), "every step taken without a failure");
            drop(run);
            disk.changes()
        };
        for k in 1..=total {
            let dir = tempfile::tempdir().unwrap();
            let disk = Disk::arm(dir.path(), Plan::Fail(k));
            let mut run = Run::new(dir.path(), &disk, true);
            scenario(&mut run);
            let failed = disk.failed().unwrap_or_else(|| "nothing".into());
            for c in run.reg.list() {
                c.close_wal();
            }
            let history = std::mem::take(&mut run.history);
            drop(run);
            drop(disk);
            check_after_failure(dir.path(), &history).unwrap_or_else(|e| panic!("change {k} of {total} failing ({failed}): {e}"));
        }
    });
}

/// Start again after failures: each value as the last step acknowledged to set it left it, or as
/// a failed step after that set it; the collection whole (`check_whole`).
fn check_after_failure(dir: &Path, history: &[(Vec<(Key, Value)>, bool)]) -> Result<(), String> {
    let _disk = Disk::arm(dir, Plan::Watch);
    let reg = load(dir)?;
    if let Some(e) = reg.unloaded().iter().find(|e| e.schema.name == NAME && !e.suspended) {
        return Err(format!("{}: set aside, not loadable", e.schema.name));
    }
    let suspended = reg.unloaded().iter().any(|e| e.schema.name == NAME);
    let col = match suspended {
        true => Some(crate::persistence::resume_collection(dir, &reg, NAME, None).map_err(|e| format!("resuming: {e}"))?),
        false => reg.get(NAME),
    };
    let got = col.as_deref().map_or_else(State::default, |c| State { suspended, ..State::of(c) });
    let mut keys = vec![Key::Exists, Key::Suspended, Key::Fields];
    for (set, _) in history {
        for (k, _) in set {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
    }
    for d in &got.docs {
        if !keys.contains(&Key::Doc(d.0.clone())) {
            return Err(format!("{}: a document no step wrote", d.0));
        }
    }
    for key in keys {
        let mut floor = key.of(&State::default());
        let mut later: Vec<Value> = Vec::new();
        for (set, acked) in history {
            for (_, v) in set.iter().filter(|(k, _)| *k == key) {
                if *acked {
                    floor = v.clone();
                    later.clear();
                } else {
                    later.push(v.clone());
                }
            }
        }
        let v = key.of(&got);
        if v != floor && !later.contains(&v) {
            return Err(format!("{key:?} is {v:?}: acknowledged {floor:?}, failed since {later:?}"));
        }
    }
    let Some(col) = col else { return strays(dir) };
    check_whole(dir, reg, col, got)
}

/// Recover from `state` with a crash at each change the recovery makes, then without a crash;
/// check each. How many recoveries were checked.
fn recover_crashing(dir: &Path, state: &CrashState, acked: &State, in_flight: Option<&State>) -> Result<usize, String> {
    state.put(dir);
    let changes = {
        let disk = Disk::arm(dir, Plan::Watch);
        drop(load(dir)?);
        disk.changes()
    };
    let mut checked = 1;
    for m in 1..=changes {
        state.put(dir);
        let disk = Disk::arm(dir, Plan::Crash(m));
        drop(load(dir));
        let at = disk.stopped();
        let states = crash_states(&disk, m);
        drop(disk);
        for st in &states {
            st.put(dir);
            check(dir, acked, in_flight).map_err(|e| format!("then a crash at recovery change {m} ({}), {}: {e}", at.as_deref().unwrap_or("-"), st.label))?;
        }
        checked += states.len();
    }
    state.put(dir);
    check(dir, acked, in_flight)?;
    Ok(checked)
}

/// Recover what is in `dir` and check it: the collection as acknowledged (or with the step under
/// way), its searches those of one segment of its documents, no stray file, writes taken; a
/// deleted one's files gone.
fn check(dir: &Path, acked: &State, in_flight: Option<&State>) -> Result<(), String> {
    let _disk = Disk::arm(dir, Plan::Watch);
    let reg = load(dir)?;
    if let Some(e) = reg.unloaded().iter().find(|e| e.schema.name == NAME && !e.suspended) {
        return Err(format!("{}: set aside, not loadable", e.schema.name));
    }
    let suspended = reg.unloaded().iter().any(|e| e.schema.name == NAME);
    let col = match suspended {
        true => Some(crate::persistence::resume_collection(dir, &reg, NAME, None).map_err(|e| format!("resuming: {e}"))?),
        false => reg.get(NAME),
    };
    let got = col.as_deref().map_or_else(State::default, |c| State { suspended, ..State::of(c) });
    if got != *acked && Some(&got) != in_flight {
        return Err(format!("recovered {got:?}; acknowledged {acked:?}; under way {in_flight:?}"));
    }
    let Some(col) = col else {
        if dir.join("collections.json").is_file() && (dir.join("indexes").join(NAME).exists() || dir.join("wal").join(NAME).exists()) {
            return Err("files of a collection collections.json does not list are left".into());
        }
        return strays(dir);
    };
    check_whole(dir, reg, col, got)
}

/// The collection recovered is whole: its log open, its searches those of one segment of its
/// documents, no stray file; it takes writes, kept over a restart.
fn check_whole(dir: &Path, reg: Arc<CollectionRegistry>, col: Arc<Collection>, got: State) -> Result<(), String> {
    let realtime = col.schema.index_mode == IndexMode::Realtime;
    if realtime && col.wal().is_none() {
        return Err("the log does not open".into());
    }
    searches(&col, &got)?;
    strays(dir)?;
    // writes again, kept over a restart
    let mut want = State { suspended: false, ..got };
    if realtime {
        col.apply_writes(vec![doc(&col.schema, "after", "after crash", 99)], &[])
            .map_err(|e| format!("a write after recovery: {e}"))?
            .expect("a logged collection")
            .wait_blocking()
            .map_err(|e| format!("a write after recovery: {e}"))?;
        want.write(&[("after", "after crash", 99)], &[]);
    }
    col.close_wal();
    drop((col, reg));
    let reg = load(dir)?;
    let again = reg.get(NAME).map_or_else(State::default, |c| State::of(&c));
    if again != want {
        return Err(format!("after a write and a restart {again:?}, not {want:?}"));
    }
    for c in reg.list() {
        c.close_wal();
    }
    Ok(())
}

/// The collection answers as one segment of its documents.
fn searches(col: &Collection, state: &State) -> Result<(), String> {
    let docs = state.docs.iter().map(|(id, title, n)| doc(&col.schema, id, title, *n)).collect();
    let one = crate::ingest::build_index(&col.schema, docs);
    let bundle = col.snapshot_bundle();
    let summary = |r: crate::search::SearchResponse| {
        let mut v = serde_json::to_value(&r).unwrap();
        v.as_object_mut().unwrap().remove("search_time_ms");
        v
    };
    for (q, filter, sort) in [("*", None, None), ("alpha", None, None), ("gamma delta", Some("n:>3"), None), ("*", None, Some("n:desc")), ("alpa", None, None)] {
        let p = SearchParams {
            q: q.into(),
            query_by: vec!["title".into()],
            filter_by: filter.map(Into::into),
            sort_by: sort.map(Into::into),
            facet_by: Some("n".into()),
            per_page: 100,
            page: 1,
            ..Default::default()
        };
        let a = summary(crate::search::execute(&one, &col.schema, &p)?);
        let b = summary(crate::search::execute(SearchTarget::Bundle(&bundle), &col.schema, &p)?);
        if a != b {
            return Err(format!("q={q} filter={filter:?} sort={sort:?}: {b} where one segment answers {a}"));
        }
    }
    Ok(())
}

/// No file the manifest does not hold, no temporary file, nothing but log files in the log.
fn strays(dir: &Path) -> Result<(), String> {
    let names = |d: PathBuf| -> Vec<String> { std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect() };
    if let Some(keep) = crate::persistence::index::manifest_files(dir, NAME) {
        if let Some(stray) = names(dir.join("indexes").join(NAME).join("segments")).into_iter().find(|n| n != "manifest.bin" && !keep.contains(n)) {
            return Err(format!("{stray}: in the segment set, not in its manifest"));
        }
    }
    if let Some(stray) = names(dir.join("wal").join(NAME)).into_iter().find(|n| !n.ends_with(".log")) {
        return Err(format!("{stray}: in the log"));
    }
    if let Some(stray) = names(dir.to_path_buf()).into_iter().find(|n| n.ends_with(".tmp")) {
        return Err(format!("{stray}: left in the data directory"));
    }
    Ok(())
}

/// Writes, a merge in memory, flushes and saves (deletion masks of flushed segments), a flushed
/// segment with many deleted copies merged alone, a restart, a truncation.
fn writes_flushes_and_merges(run: &mut Run) {
    run.create();
    run.write(&[("a", "alpha one", 1), ("b", "beta two", 2), ("c", "gamma three", 3)], &[]);
    run.write(&[("d", "delta four", 4)], &["b"]);
    run.flush();
    run.write(&[("a", "alpha again", 10), ("e", "epsilon five", 5)], &[]);
    run.write(&[], &["c"]);
    run.save();
    for i in 0..10 {
        let id = format!("m{i}");
        run.write(&[(&id, "mu gamma", i)], &[]);
    }
    run.merge_memory();
    run.flush();
    // a quarter of the flushed segment written again: it merges alone
    run.write(&[("m0", "mu again", 100), ("m1", "mu again", 101), ("m2", "mu again", 102)], &[]);
    run.reclaim();
    run.write(&[("a", "alpha third", 11), ("d", "delta again", 40)], &["e"]);
    run.flush();
    run.restart();
    run.write(&[("f", "phi six", 6)], &[]);
    run.truncate();
    run.write(&[("g", "gamma seven", 7), ("a", "alpha back", 12)], &[]);
    run.flush();
}

/// Flushes enough for flushed segments of one size to merge, deletions among them.
fn tiers_of_flushed_segments(run: &mut Run) {
    run.create();
    for i in 0..11 {
        let id = format!("t{i}");
        let deletes: &[&str] = if i % 4 == 3 { &["t0"] } else { &[] };
        run.write(&[(&id, "tau delta", i), ("t1", "tau again", 100 + i)], deletes);
        run.flush();
    }
}

/// A schema change between writes and flushes: the collection made again, its manifest the
/// commit, collections.json after.
fn a_schema_change(run: &mut Run) {
    run.create();
    run.write(&[("a", "alpha one", 1), ("b", "beta two", 2)], &[]);
    run.flush();
    run.write(&[("c", "gamma three", 3), ("a", "alpha again", 10)], &["b"]);
    run.add_field();
    run.write(&[("d", "delta four", 4)], &[]);
    run.flush();
}

/// A collection deleted, its files removed, and one made again under the name.
fn delete_and_create_again(run: &mut Run) {
    run.create();
    run.write(&[("a", "alpha one", 1), ("b", "beta two", 2)], &[]);
    run.flush();
    run.write(&[("c", "gamma three", 3)], &["a"]);
    run.delete();
    run.create();
    run.write(&[("d", "delta four", 4)], &[]);
    run.flush();
}

/// A collection suspended (saved, its log closed, set aside) and resumed.
fn suspend_and_resume(run: &mut Run) {
    run.create();
    run.write(&[("a", "alpha one", 1), ("b", "beta two", 2)], &[]);
    run.flush();
    run.write(&[("c", "gamma three", 3)], &["a"]);
    run.suspend();
    run.resume();
    run.write(&[("d", "delta four", 4)], &[]);
    run.flush();
}

/// The nightly rebuild: a collection installed from an upload, deleted, installed again.
fn uploads(run: &mut Run) {
    run.install(&[("a", "alpha one", 1), ("b", "beta two", 2), ("c", "gamma delta", 30)]);
    run.delete();
    run.install(&[("b", "beta again", 20), ("d", "delta four", 4)]);
}

/// What an upgrade finds: a realtime collection (a segment and a log) whose segment files are of
/// another format version, written before any disk watches.
fn collection_of_another_version(dir: &Path) {
    let reg = CollectionRegistry::new(Some(dir.to_path_buf()));
    let col = crate::persistence::create_collection(dir, &reg, schema()).unwrap();
    let docs = vec![doc(&col.schema, "a", "alpha one", 1)];
    col.apply_writes(docs, &[]).unwrap().unwrap().wait_blocking().unwrap();
    compaction::flush(&col, Some(dir), &crate::observability::MetricsCollector::new());
    settle(&col);
    col.close_wal();
    drop((col, reg));
    crate::persistence::index::forget_under(dir);
    let segments = dir.join("indexes").join(NAME).join("segments");
    let mut stamped = 0;
    for e in std::fs::read_dir(&segments).unwrap().flatten() {
        if e.path().extension().is_some_and(|x| x == "tqbf") {
            let mut bytes = std::fs::read(e.path()).unwrap();
            bytes[4..6].copy_from_slice(&5u16.to_le_bytes());
            std::fs::write(e.path(), bytes).unwrap();
            stamped += 1;
        }
    }
    assert!(stamped > 0, "a segment file");
}

/// Set aside with every file it had, or gone with none of them: and then the name takes a new
/// collection.
fn check_set_aside_or_gone(dir: &Path) -> Result<(), String> {
    let _disk = Disk::arm(dir, Plan::Watch);
    let reg = load(dir)?;
    if reg.unloaded().iter().any(|e| e.schema.name == NAME) {
        if !dir.join("wal").join(NAME).exists() {
            return Err("set aside without its log".into());
        }
        crate::persistence::delete_set_aside(dir, &reg, NAME).map_err(|e| format!("deleting it after the restart: {e}"))?;
    }
    if dir.join("indexes").join(NAME).exists() || dir.join("wal").join(NAME).exists() {
        return Err("files of a deleted collection are left".into());
    }
    let col = crate::persistence::create_collection(dir, &reg, schema()).map_err(|e| format!("the name is not free: {e}"))?;
    col.close_wal();
    Ok(())
}

#[test]
fn a_collection_of_another_version_is_set_aside_and_deleted_whatever_a_crash_cuts_short() {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    pool.install(|| {
        let total = {
            let dir = tempfile::tempdir().unwrap();
            collection_of_another_version(dir.path());
            let disk = Disk::arm(dir.path(), Plan::Watch);
            let reg = load(dir.path()).unwrap();
            assert!(reg.get(NAME).is_none() && reg.unloaded().iter().any(|e| e.schema.name == NAME), "set aside at the start");
            let entry = crate::persistence::delete_set_aside(dir.path(), &reg, NAME).unwrap();
            assert_eq!(entry.schema.name, NAME);
            assert!(reg.unloaded().is_empty());
            disk.changes()
        };
        assert!(total > 0, "a deletion changes the data directory");
        for k in 1..=total {
            for crash in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                collection_of_another_version(dir.path());
                let disk = Disk::arm(dir.path(), if crash { Plan::Crash(k) } else { Plan::Fail(k) });
                if let Ok(reg) = load(dir.path()) {
                    let deleted = crate::persistence::delete_set_aside(dir.path(), &reg, NAME);
                    let aside = reg.unloaded().iter().any(|e| e.schema.name == NAME);
                    assert_eq!(aside, deleted.is_err(), "change {k}: set aside exactly when the deletion failed");
                }
                if crash {
                    let at = disk.stopped().unwrap_or_else(|| panic!("change {k} of {total}: the disk did not stop"));
                    let states = crash_states(&disk, k);
                    drop(disk);
                    for st in &states {
                        st.put(dir.path());
                        check_set_aside_or_gone(dir.path()).unwrap_or_else(|e| panic!("a crash at change {k} of {total} ({at}), {}: {e}", st.label));
                    }
                } else {
                    let failed = disk.failed().unwrap_or_else(|| "nothing".into());
                    drop(disk);
                    check_set_aside_or_gone(dir.path()).unwrap_or_else(|e| panic!("change {k} of {total} failing ({failed}): {e}"));
                }
            }
        }
    });
}

#[test]
fn a_suspended_collection_is_not_deleted_as_set_aside() {
    let dir = tempfile::tempdir().unwrap();
    let reg = CollectionRegistry::new(Some(dir.path().to_path_buf()));
    crate::persistence::create_collection(dir.path(), &reg, schema()).unwrap();
    crate::persistence::suspend_collection(dir.path(), &reg, NAME).unwrap();
    let refused = crate::persistence::delete_set_aside(dir.path(), &reg, NAME);
    assert!(matches!(refused, Err(CollectionError::NotFound(_))));
    assert!(reg.unloaded().iter().any(|e| e.schema.name == NAME && e.suspended));
    assert!(dir.path().join("wal").join(NAME).exists(), "its files kept");
}

#[test]
fn a_crash_anywhere_in_writes_flushes_and_merges_loses_nothing_acknowledged() {
    let done = crash_everywhere(writes_flushes_and_merges);
    assert!(done.merges >= 2, "a merge in memory and a reclaim");
}

#[test]
fn a_crash_anywhere_in_merges_of_flushed_segments_loses_nothing_acknowledged() {
    let done = crash_everywhere(tiers_of_flushed_segments);
    assert!(done.segments < 11, "flushed segments merged: {}", done.segments);
}

#[test]
fn a_crash_anywhere_in_a_schema_change_loses_nothing_acknowledged() {
    crash_everywhere(a_schema_change);
}

#[test]
fn a_crash_anywhere_in_a_deletion_leaves_no_file_of_it() {
    crash_everywhere(delete_and_create_again);
}

#[test]
fn a_crash_anywhere_in_a_suspension_loses_nothing_acknowledged() {
    crash_everywhere(suspend_and_resume);
}

#[test]
fn a_crash_anywhere_in_an_upload_leaves_the_collection_whole_or_none() {
    crash_everywhere(uploads);
}

#[test]
fn a_failure_anywhere_in_writes_flushes_and_merges_loses_nothing_acknowledged() {
    fail_everywhere(writes_flushes_and_merges);
}

#[test]
fn a_failure_anywhere_in_merges_of_flushed_segments_loses_nothing_acknowledged() {
    fail_everywhere(tiers_of_flushed_segments);
}

#[test]
fn a_failure_anywhere_in_a_schema_change_loses_nothing_acknowledged() {
    fail_everywhere(a_schema_change);
}

#[test]
fn a_failure_anywhere_in_a_deletion_loses_nothing_acknowledged() {
    fail_everywhere(delete_and_create_again);
}

#[test]
fn a_failure_anywhere_in_a_suspension_loses_nothing_acknowledged() {
    fail_everywhere(suspend_and_resume);
}

#[test]
fn a_failure_anywhere_in_an_upload_loses_nothing_acknowledged() {
    fail_everywhere(uploads);
}
