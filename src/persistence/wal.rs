//! A realtime collection's write-ahead log. Every write is one record, numbered by its log
//! sequence number (LSN) in the order the writes apply, and acknowledged only once on disk: one
//! writer thread appends what is queued and syncs once for all of it, so the writes queued while a
//! sync runs share the next one (group commit). The bundle saved last holds every write up to its
//! LSN (`IndexBundle::lsn`, the checkpoint in its manifest); a restart replays the records after
//! it, and a checkpoint removes the files that hold nothing after it.
//!
//! Layout: `wal/{collection}/{first_lsn:020}.log`, records `[u32 len][u32 crc32 of body][body]`,
//! a body `[u64 lsn][u64 schema_version][u8 kind]` and, for writes, the paths the collection's
//! field registry gained since the record before (`tqbf::registry`: a record's ids mean nothing
//! without them), then each write's documents (each as written: its record, every path; or typed
//! fields for one that came without JSON) and deleted ids: a record holds the writes a collection
//! applied together. A crash can tear only the last record of the last file: recovery cuts the
//! file there. A sync that fails fails its writes and every later one (the file may end in a
//! partial record), until a restart recovers what is on disk.

use super::disk;
use crate::tqbf::registry::FieldEntry;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::thread::JoinHandle;

/// A file is closed and the next one begun once it holds this much.
const FILE_BYTES: u64 = 64 << 20;
/// Larger than any record: a length beyond it is a torn one.
const MAX_RECORD: u32 = 1 << 30;

const WRITE: u8 = 1;
const CLEAR: u8 = 2;

const RECORD: u8 = 1;
const TYPED: u8 = 2;

pub enum Entry {
    /// Writes applied together, in order (`Collection::apply_writes`), and the paths the field
    /// registry gained since the record before (with their ids), which their records may use.
    Writes { paths: Vec<(u16, FieldEntry)>, ops: Vec<WriteOp> },
    /// Every document deleted (`Collection::truncate`).
    Clear,
}

/// One write: documents written, then ids deleted.
pub struct WriteOp {
    pub upserts: Vec<Logged>,
    pub deletes: Vec<String>,
}

/// A document as logged.
#[derive(Clone, Debug, PartialEq)]
pub enum Logged {
    /// As written: its record (`tqbf::record`), every path of it.
    Record(std::sync::Arc<[u8]>),
    /// Typed fields (`ingest::protocol::serialize_document`): one that came without JSON (typed
    /// TCP ingest).
    Typed(Vec<u8>),
}

impl Logged {
    fn kind_and_bytes(&self) -> (u8, &[u8]) {
        match self {
            Logged::Record(r) => (RECORD, r),
            Logged::Typed(t) => (TYPED, t),
        }
    }
}

pub struct Record {
    pub lsn: u64,
    /// `Collection::schema_version` of the collection written: its documents are typed under it.
    pub schema_version: u64,
    pub entry: Entry,
}

enum Request {
    Append { lsn: u64, frame: Vec<u8>, done: Vec<tokio::sync::oneshot::Sender<Result<(), String>>> },
    Checkpoint(u64),
    Barrier(tokio::sync::oneshot::Sender<()>),
}

/// A record's acknowledgement: Ok once it is on disk.
pub struct Commit(tokio::sync::oneshot::Receiver<Result<(), String>>);

impl Commit {
    pub async fn wait(self) -> Result<(), String> {
        self.0.await.unwrap_or_else(|_| Err(closed()))
    }

    /// From a thread outside the async runtime.
    pub fn wait_blocking(self) -> Result<(), String> {
        self.0.blocking_recv().unwrap_or_else(|_| Err(closed()))
    }
}

fn closed() -> String {
    "The write-ahead log is closed.".into()
}

pub struct Wal {
    dir: PathBuf,
    /// The writer's queue and the next LSN, under one lock: records reach the writer in the order
    /// of their LSNs.
    queue: Mutex<Queue>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

struct Queue {
    tx: Option<mpsc::Sender<Request>>,
    next_lsn: u64,
}

pub fn wal_dir(data_dir: &Path, collection: &str) -> PathBuf {
    data_dir.join("wal").join(collection)
}

/// Bytes the collection's log holds on disk.
pub fn disk_size(data_dir: &Path, collection: &str) -> u64 {
    files(&wal_dir(data_dir, collection)).map(|f| f.iter().filter_map(|(_, p)| std::fs::metadata(p).ok()).map(|m| m.len()).sum()).unwrap_or(0)
}

/// Remove the collection's log for good (the collection is deleted, or made anew): its removal
/// durable.
pub fn remove(data_dir: &Path, collection: &str) -> io::Result<()> {
    let dir = wal_dir(data_dir, collection);
    match disk::remove_dir_all(&dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        Err(_) => Ok(()),
        Ok(()) => disk::sync_parent(&dir),
    }
}

impl Wal {
    /// A new collection's log: whatever a collection of the name left is removed.
    pub fn create(data_dir: &Path, collection: &str) -> io::Result<Wal> {
        remove(data_dir, collection)?;
        Ok(Self::open(data_dir, collection, 0)?.0)
    }

    /// Open the collection's log and read the records after `checkpoint` (the LSN its saved
    /// bundle holds), in LSN order. A torn last record is cut off.
    pub fn open(data_dir: &Path, collection: &str, checkpoint: u64) -> io::Result<(Wal, Vec<Record>)> {
        let dir = wal_dir(data_dir, collection);
        disk::create_dirs(&dir)?;
        let mut found = files(&dir)?;
        let mut records = Vec::new();
        let mut last_lsn = checkpoint;
        let count = found.len();
        for (i, (_, path)) in found.iter().enumerate() {
            let (file_records, valid, torn) = read_file(path)?;
            if torn {
                if i + 1 < count {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{}: a damaged record before the last file", path.display())));
                }
                tracing::warn!(collection, file = %path.display(), valid_bytes = valid, "Cut a torn record off the end of the write-ahead log");
                disk::cut(path, valid)?;
            }
            for r in file_records {
                if r.lsn <= last_lsn && r.lsn > checkpoint {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{}: LSN {} out of order", path.display(), r.lsn)));
                }
                last_lsn = last_lsn.max(r.lsn);
                if r.lsn > checkpoint {
                    records.push(r);
                }
            }
        }
        let next_lsn = last_lsn + 1;
        let (file, path, bytes) = match found.last() {
            Some((_, path)) if std::fs::metadata(path)?.len() < FILE_BYTES => {
                let file = OpenOptions::new().append(true).open(path)?;
                let bytes = file.metadata()?.len();
                (file, path.clone(), bytes)
            }
            _ => {
                let path = dir.join(file_name(next_lsn));
                let file = disk::create_new_append(&path)?;
                disk::sync_dir(&dir)?;
                found.push((next_lsn, path.clone()));
                (file, path, 0)
            }
        };
        let (tx, rx) = mpsc::channel();
        let writer = Writer { dir: dir.clone(), file, path, bytes, files: found.into_iter().map(|f| f.0).collect(), last_lsn, failed: None };
        let thread = std::thread::Builder::new().name(format!("wal-{collection}")).spawn(move || writer.run(rx))?;
        let wal = Wal { dir, queue: Mutex::new(Queue { tx: Some(tx), next_lsn }), thread: Mutex::new(Some(thread)) };
        Ok((wal, records))
    }

    /// Append a record; its LSN and its acknowledgement. Fails once the log is closed. Records are
    /// numbered and queued in the order of the calls.
    pub fn append(&self, schema_version: u64, entry: &Entry) -> Result<(u64, Commit), String> {
        let (lsn, mut commits) = self.append_for(schema_version, entry, 1)?;
        Ok((lsn, commits.pop().expect("one acknowledgement")))
    }

    /// `append`, acknowledged to `waiters` (the writes a record holds, each answered).
    pub fn append_for(&self, schema_version: u64, entry: &Entry, waiters: usize) -> Result<(u64, Vec<Commit>), String> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let lsn = queue.next_lsn;
        let tx = queue.tx.as_ref().ok_or_else(closed)?;
        let (done, commits): (Vec<_>, Vec<_>) = (0..waiters).map(|_| tokio::sync::oneshot::channel()).map(|(s, r)| (s, Commit(r))).unzip();
        tx.send(Request::Append { lsn, frame: frame(lsn, schema_version, entry), done }).map_err(|_| closed())?;
        queue.next_lsn += 1;
        Ok((lsn, commits))
    }

    /// The bundle holding every write up to `lsn` is saved: the files holding nothing after it go.
    pub fn checkpoint(&self, lsn: u64) {
        if let Some(tx) = self.queue.lock().unwrap_or_else(|e| e.into_inner()).tx.as_ref() {
            let _ = tx.send(Request::Checkpoint(lsn));
        }
    }

    /// Resolves once every record appended before is on disk (or failed).
    pub async fn barrier(&self) {
        let rx = {
            let queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            let Some(tx) = queue.tx.as_ref() else { return };
            let (ack, rx) = tokio::sync::oneshot::channel();
            if tx.send(Request::Barrier(ack)).is_err() {
                return;
            }
            rx
        };
        let _ = rx.await;
    }

    /// Stop the writer once it has written everything appended; appends fail from here on.
    pub fn shutdown(&self) {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).tx = None;
        if let Some(thread) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = thread.join();
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Wal {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Writer {
    dir: PathBuf,
    file: File,
    /// `file`'s name.
    path: PathBuf,
    /// Bytes in `file`.
    bytes: u64,
    /// The files' first LSNs, in order; the last one is `file`.
    files: Vec<u64>,
    last_lsn: u64,
    failed: Option<String>,
}

impl Writer {
    fn run(mut self, rx: mpsc::Receiver<Request>) {
        while let Ok(first) = rx.recv() {
            let mut buf = Vec::new();
            let mut done = Vec::new();
            let mut checkpoint = None;
            let mut barriers = Vec::new();
            let mut take = |r: Request, last_lsn: &mut u64| match r {
                Request::Append { lsn, frame, done: d } => {
                    buf.extend_from_slice(&frame);
                    done.extend(d);
                    *last_lsn = lsn;
                }
                Request::Checkpoint(lsn) => checkpoint = Some(checkpoint.map_or(lsn, |c: u64| c.max(lsn))),
                Request::Barrier(ack) => barriers.push(ack),
            };
            let mut last_lsn = self.last_lsn;
            take(first, &mut last_lsn);
            while let Ok(r) = rx.try_recv() {
                take(r, &mut last_lsn);
            }
            // A checkpoint begins a new file before the group is written, so the files before it
            // hold nothing written after the checkpoint was asked for
            if checkpoint.is_some() && self.bytes > 0 && self.failed.is_none() {
                self.rotate();
            }
            let result = match &self.failed {
                Some(e) => Err(e.clone()),
                None if buf.is_empty() => Ok(()),
                None => self.file.write_all(&buf).and_then(|_| disk::sync_data(&self.file, &self.path)).map_err(|e| format!("The write-ahead log could not write to disk: {e}")),
            };
            match &result {
                Ok(()) => {
                    self.bytes += buf.len() as u64;
                    self.last_lsn = last_lsn;
                }
                Err(e) => self.fail(e.clone()),
            }
            for d in done {
                let _ = d.send(result.clone());
            }
            if self.failed.is_none() {
                if let Some(lsn) = checkpoint {
                    self.remove_through(lsn);
                }
                if self.bytes >= FILE_BYTES {
                    self.rotate();
                }
            }
            for ack in barriers {
                let _ = ack.send(());
            }
        }
    }

    /// Refuse every write from here on: one may not be on disk.
    fn fail(&mut self, e: String) {
        if self.failed.is_none() {
            tracing::error!(dir = %self.dir.display(), error = %e, "Write-ahead log failed: writes are refused until a restart");
            self.failed = Some(e);
        }
    }

    /// Continue in a new file, its first record the next LSN. One that cannot be made leaves the
    /// records in the current one; one whose name a crash may not keep fails the log (its records
    /// would go with it).
    fn rotate(&mut self) {
        let first = self.last_lsn + 1;
        let path = self.dir.join(file_name(first));
        match disk::create_new_append(&path) {
            Ok(file) => {
                if let Err(e) = disk::sync_dir(&self.dir) {
                    self.fail(format!("The write-ahead log could not begin a new file: {e}"));
                    return;
                }
                self.file = file;
                self.path = path;
                self.bytes = 0;
                self.files.push(first);
            }
            Err(e) => tracing::warn!(file = %path.display(), error = %e, "Write-ahead log could not begin a new file, goes on in the current one"),
        }
    }

    /// Remove every file whose records are all at or before `lsn` (never the current one).
    fn remove_through(&mut self, lsn: u64) {
        while self.files.len() > 1 && self.files[1] - 1 <= lsn {
            let path = self.dir.join(file_name(self.files[0]));
            if let Err(e) = disk::remove_file(&path) {
                tracing::warn!(file = %path.display(), error = %e, "Write-ahead log could not remove a checkpointed file");
                break;
            }
            self.files.remove(0);
        }
    }
}

fn file_name(first_lsn: u64) -> String {
    format!("{first_lsn:020}.log")
}

/// The log's files by their first LSN, in order.
fn files(dir: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(found),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "log")
            && let Some(lsn) = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u64>().ok())
        {
            found.push((lsn, path));
        }
    }
    found.sort_by_key(|f| f.0);
    Ok(found)
}

fn frame(lsn: u64, schema_version: u64, entry: &Entry) -> Vec<u8> {
    let mut body = Vec::with_capacity(17 + match entry {
        Entry::Writes { paths, ops } => {
            8 + paths.iter().map(|(_, p)| 9 + p.name.len()).sum::<usize>()
                + ops.iter().map(|op| 8 + op.upserts.iter().map(|u| u.kind_and_bytes().1.len() + 5).sum::<usize>() + op.deletes.iter().map(|d| d.len() + 4).sum::<usize>()).sum::<usize>()
        }
        Entry::Clear => 0,
    });
    body.extend_from_slice(&lsn.to_le_bytes());
    body.extend_from_slice(&schema_version.to_le_bytes());
    match entry {
        Entry::Writes { paths, ops } => {
            body.push(WRITE);
            body.extend_from_slice(&(paths.len() as u32).to_le_bytes());
            for (id, p) in paths {
                body.extend_from_slice(&id.to_le_bytes());
                body.extend_from_slice(&p.parent.to_le_bytes());
                body.extend_from_slice(&p.flags.to_le_bytes());
                body.push(p.value_type);
                body.extend_from_slice(&(p.name.len() as u16).to_le_bytes());
                body.extend_from_slice(p.name.as_bytes());
            }
            body.extend_from_slice(&(ops.len() as u32).to_le_bytes());
            for op in ops {
                body.extend_from_slice(&(op.upserts.len() as u32).to_le_bytes());
                for doc in &op.upserts {
                    let (kind, bytes) = doc.kind_and_bytes();
                    body.push(kind);
                    body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                    body.extend_from_slice(bytes);
                }
                body.extend_from_slice(&(op.deletes.len() as u32).to_le_bytes());
                for id in &op.deletes {
                    body.extend_from_slice(&(id.len() as u32).to_le_bytes());
                    body.extend_from_slice(id.as_bytes());
                }
            }
        }
        Entry::Clear => body.push(CLEAR),
    }
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(&body).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// The records of a file, how many of its bytes are whole records, and whether a torn or damaged
/// record follows them.
fn read_file(path: &Path) -> io::Result<(Vec<Record>, u64, bool)> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    let mut records = Vec::new();
    let mut at = 0usize;
    while at < data.len() {
        let Some(head) = data.get(at..at + 8) else { return Ok((records, at as u64, true)) };
        let len = u32::from_le_bytes(head[0..4].try_into().unwrap());
        let crc = u32::from_le_bytes(head[4..8].try_into().unwrap());
        if len > MAX_RECORD {
            return Ok((records, at as u64, true));
        }
        let Some(body) = data.get(at + 8..at + 8 + len as usize) else { return Ok((records, at as u64, true)) };
        if crc32fast::hash(body) != crc {
            return Ok((records, at as u64, true));
        }
        match parse(body) {
            Some(r) => records.push(r),
            None => return Ok((records, at as u64, true)),
        }
        at += 8 + len as usize;
    }
    Ok((records, at as u64, false))
}

fn parse(body: &[u8]) -> Option<Record> {
    let mut r = Reader { data: body, at: 0 };
    let lsn = r.u64()?;
    let schema_version = r.u64()?;
    let entry = match r.u8()? {
        WRITE => {
            let n = r.u32()?;
            let mut paths = Vec::with_capacity(n.min(1 << 16) as usize);
            for _ in 0..n {
                let id = r.u16()?;
                let (parent, flags, value_type) = (r.u16()?, r.u16()?, r.u8()?);
                let len = r.u16()? as usize;
                let name = std::str::from_utf8(r.bytes(len)?).ok()?.into();
                paths.push((id, FieldEntry { parent, flags, value_type, name }));
            }
            let writes = r.u32()?;
            let mut ops = Vec::with_capacity(writes.min(1 << 16) as usize);
            for _ in 0..writes {
                let n = r.u32()?;
                let mut upserts = Vec::with_capacity(n.min(1 << 16) as usize);
                for _ in 0..n {
                    let kind = r.u8()?;
                    let len = r.u32()? as usize;
                    let bytes = r.bytes(len)?;
                    upserts.push(match kind {
                        RECORD => Logged::Record(bytes.into()),
                        TYPED => Logged::Typed(bytes.to_vec()),
                        _ => return None,
                    });
                }
                let n = r.u32()?;
                let mut deletes = Vec::with_capacity(n.min(1 << 16) as usize);
                for _ in 0..n {
                    let len = r.u32()? as usize;
                    deletes.push(String::from_utf8(r.bytes(len)?.to_vec()).ok()?);
                }
                ops.push(WriteOp { upserts, deletes });
            }
            Entry::Writes { paths, ops }
        }
        CLEAR => Entry::Clear,
        _ => return None,
    };
    (r.at == body.len()).then_some(Record { lsn, schema_version, entry })
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let b = self.data.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(b)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.bytes(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(n: u32) -> Entry {
        Entry::Writes { paths: Vec::new(), ops: vec![WriteOp { upserts: vec![Logged::Typed(format!("doc{n}").into_bytes())], deletes: vec![format!("gone{n}")] }] }
    }

    fn upserts(r: &Record) -> Vec<String> {
        match &r.entry {
            Entry::Writes { ops, .. } => ops.iter().flat_map(|op| op.upserts.iter().map(|u| String::from_utf8(u.kind_and_bytes().1.to_vec()).unwrap())).collect(),
            Entry::Clear => vec!["<clear>".into()],
        }
    }

    #[test]
    fn a_record_of_several_writes_keeps_their_order_and_answers_each() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::create(tmp.path(), "c").unwrap();
        let ops = vec![
            WriteOp { upserts: vec![], deletes: vec!["x".into()] },
            WriteOp { upserts: vec![Logged::Record(b"x".to_vec().into())], deletes: vec![] },
        ];
        let path = FieldEntry { parent: crate::tqbf::registry::NO_PARENT, flags: 1, value_type: 2, name: "title".into() };
        let (lsn, commits) = wal.append_for(0, &Entry::Writes { paths: vec![(3, path.clone())], ops }, 2).unwrap();
        assert_eq!((lsn, commits.len()), (1, 2));
        for c in commits {
            c.wait_blocking().unwrap();
        }
        wal.shutdown();
        let (_, records) = Wal::open(tmp.path(), "c", 0).unwrap();
        let Entry::Writes { paths, ops } = &records[0].entry else { panic!("writes") };
        assert_eq!(paths, &[(3, path)]);
        assert_eq!((ops[0].deletes.clone(), ops[1].upserts.clone()), (vec!["x".to_string()], vec![Logged::Record(b"x".to_vec().into())]));
    }

    #[test]
    fn records_are_read_back_after_the_checkpoint_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::create(tmp.path(), "c").unwrap();
        for n in 1..=5 {
            let (lsn, commit) = wal.append(7, &write(n)).unwrap();
            assert_eq!(lsn, n as u64);
            commit.wait_blocking().unwrap();
        }
        wal.append(7, &Entry::Clear).unwrap().1.wait_blocking().unwrap();
        wal.shutdown();
        drop(wal);

        let (wal, records) = Wal::open(tmp.path(), "c", 2).unwrap();
        assert_eq!(records.iter().map(|r| r.lsn).collect::<Vec<_>>(), [3, 4, 5, 6]);
        assert!(records.iter().all(|r| r.schema_version == 7));
        assert_eq!(upserts(&records[0]), ["doc3"]);
        assert_eq!(upserts(&records[3]), ["<clear>"]);
        let (lsn, commit) = wal.append(7, &write(9)).unwrap();
        assert_eq!(lsn, 7, "numbering goes on after the last record");
        commit.wait_blocking().unwrap();
    }

    #[test]
    fn numbering_goes_on_after_a_checkpoint_that_removed_every_record() {
        let tmp = tempfile::tempdir().unwrap();
        let (wal, records) = Wal::open(tmp.path(), "c", 41).unwrap();
        assert!(records.is_empty());
        assert_eq!(wal.append(0, &write(1)).unwrap().0, 42);
    }

    #[test]
    fn a_torn_last_record_is_cut_off_and_appending_goes_on_after_the_whole_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::create(tmp.path(), "c").unwrap();
        for n in 1..=3 {
            wal.append(0, &write(n)).unwrap().1.wait_blocking().unwrap();
        }
        wal.shutdown();
        let (_, path) = files(&wal_dir(tmp.path(), "c")).unwrap().pop().unwrap();
        drop(wal);
        let len = std::fs::metadata(&path).unwrap().len();
        OpenOptions::new().write(true).open(&path).unwrap().set_len(len - 5).unwrap();

        let (wal, records) = Wal::open(tmp.path(), "c", 0).unwrap();
        assert_eq!(records.iter().map(|r| r.lsn).collect::<Vec<_>>(), [1, 2]);
        wal.append(0, &write(4)).unwrap().1.wait_blocking().unwrap();
        wal.shutdown();
        drop(wal);
        let (_, records) = Wal::open(tmp.path(), "c", 0).unwrap();
        assert_eq!(records.iter().map(|r| r.lsn).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(upserts(&records[2]), ["doc4"]);
    }

    #[test]
    fn a_damaged_record_in_the_middle_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::create(tmp.path(), "c").unwrap();
        wal.append(0, &write(1)).unwrap().1.wait_blocking().unwrap();
        wal.checkpoint(0);
        wal.append(0, &write(2)).unwrap().1.wait_blocking().unwrap();
        wal.shutdown();
        drop(wal);
        let found = files(&wal_dir(tmp.path(), "c")).unwrap();
        assert_eq!(found.len(), 2, "a checkpoint begins a new file");
        let mut data = std::fs::read(&found[0].1).unwrap();
        let last = data.len() - 1;
        data[last] ^= 0xff;
        std::fs::write(&found[0].1, data).unwrap();
        assert!(Wal::open(tmp.path(), "c", 0).is_err());
    }

    #[test]
    fn a_checkpoint_removes_the_files_it_covers() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::create(tmp.path(), "c").unwrap();
        let dir = wal_dir(tmp.path(), "c");
        for n in 1..=3 {
            wal.append(0, &write(n)).unwrap().1.wait_blocking().unwrap();
        }
        // a barrier is answered after the checkpoints queued before it (acknowledgements may not be)
        let settle = |wal: &Wal| tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(wal.barrier());
        wal.checkpoint(2);
        wal.append(0, &write(4)).unwrap().1.wait_blocking().unwrap();
        settle(&wal);
        assert_eq!(files(&dir).unwrap().len(), 2, "records 1-3 hold 3, after the checkpoint");
        wal.checkpoint(3);
        wal.append(0, &write(5)).unwrap().1.wait_blocking().unwrap();
        settle(&wal);
        let left: Vec<u64> = files(&dir).unwrap().into_iter().map(|f| f.0).collect();
        assert_eq!(left, [4, 5], "the first file went; the one holding 4 waits for a checkpoint at 4");
        wal.shutdown();
        drop(wal);
        let (_, records) = Wal::open(tmp.path(), "c", 3).unwrap();
        assert_eq!(records.iter().map(|r| r.lsn).collect::<Vec<_>>(), [4, 5]);
    }

    #[test]
    fn concurrent_writes_share_syncs_and_are_all_acknowledged_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = std::sync::Arc::new(Wal::create(tmp.path(), "c").unwrap());
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let wal = std::sync::Arc::clone(&wal);
                std::thread::spawn(move || {
                    for n in 0..200 {
                        wal.append(0, &write(t * 1000 + n)).unwrap().1.wait_blocking().unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        wal.shutdown();
        let (_, records) = Wal::open(tmp.path(), "c", 0).unwrap();
        assert_eq!(records.len(), 1600);
        assert!(records.windows(2).all(|w| w[1].lsn == w[0].lsn + 1));
    }

    #[test]
    fn appends_fail_once_shut_down() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::create(tmp.path(), "c").unwrap();
        wal.shutdown();
        assert!(wal.append(0, &write(1)).is_err());
    }
}
