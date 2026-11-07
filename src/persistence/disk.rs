//! The changes Torque makes to its data directory: files created, synced, cut, renamed and removed,
//! directories made, removed and synced, each the std call. Everything a restart has to find on
//! disk goes through here, so the lib's tests can stop the disk before any one of them and put on
//! it what a crash at that point could leave (`fault`).
//!
//! What a crash keeps: a file's data once synced; a name (a file created, renamed or removed, a
//! directory made or removed) on Unix once its directory is synced, on NTFS once a later sync
//! flushes the journal, which holds the names in the order they were made.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// One change, as the tests see it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Op<'a> {
    Create,
    Sync,
    Cut,
    Rename(&'a Path),
    Remove,
    RemoveTree,
    MakeDirs,
    SyncDir,
}

#[inline]
fn run<T>(op: Op<'_>, path: &Path, f: impl FnOnce() -> io::Result<T>, skipped: T) -> io::Result<T> {
    #[cfg(test)]
    {
        let pending = fault::before(op, path)?;
        let r = if pending.real() { f() } else { Ok(skipped) };
        pending.after(r.is_ok());
        r
    }
    #[cfg(not(test))]
    {
        let _ = (op, path, skipped);
        f()
    }
}

/// A new empty file, or the file of the name cut to nothing.
pub fn create(path: &Path) -> io::Result<File> {
    #[cfg(test)]
    {
        let pending = fault::before(Op::Create, path)?;
        let r = File::create(path);
        pending.after(r.is_ok());
        r
    }
    #[cfg(not(test))]
    File::create(path)
}

/// A new empty file to append to; an error when one of the name is there.
pub fn create_new_append(path: &Path) -> io::Result<File> {
    let open = || OpenOptions::new().create_new(true).append(true).open(path);
    #[cfg(test)]
    {
        let pending = fault::before(Op::Create, path)?;
        let r = open();
        pending.after(r.is_ok());
        r
    }
    #[cfg(not(test))]
    open()
}

/// Sync a file's data and metadata (`path`: its name).
pub fn sync(file: &File, path: &Path) -> io::Result<()> {
    run(Op::Sync, path, || file.sync_all(), ())
}

/// Sync a file's data and what reading it back needs (its length).
pub fn sync_data(file: &File, path: &Path) -> io::Result<()> {
    run(Op::Sync, path, || file.sync_data(), ())
}

/// Cut a file to `len` bytes.
pub fn cut(path: &Path, len: u64) -> io::Result<()> {
    run(Op::Cut, path, || OpenOptions::new().write(true).open(path)?.set_len(len), ())
}

pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    run(Op::Rename(to), from, || std::fs::rename(from, to), ())
}

pub fn remove_file(path: &Path) -> io::Result<()> {
    run(Op::Remove, path, || std::fs::remove_file(path), ())
}

/// Remove a directory and everything in it.
pub fn remove_dir_all(path: &Path) -> io::Result<()> {
    run(Op::RemoveTree, path, || remove_tree(path), ())
}

/// Make `path` and the directories above it that are missing, each one's name made durable in its
/// parent (`sync_dir`).
pub fn create_dirs(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    let mut missing = Vec::new();
    let mut at = Some(path);
    while let Some(dir) = at.filter(|d| !d.as_os_str().is_empty() && !d.is_dir()) {
        missing.push(dir);
        at = dir.parent();
    }
    run(Op::MakeDirs, path, || std::fs::create_dir_all(path), ())?;
    for dir in missing.iter().rev() {
        sync_parent(dir)?;
    }
    Ok(())
}

/// Make the names created, renamed or removed in `dir` durable: on Unix by syncing the directory.
/// NTFS journals names in the order they are made, and a file's sync flushes the journal through
/// the file's own changes.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    run(
        Op::SyncDir,
        dir,
        || {
            #[cfg(unix)]
            File::open(dir)?.sync_all()?;
            Ok(())
        },
        (),
    )
}

/// Make the name `path` was given (made, or renamed to) durable, with the names made before it in
/// its directory: on Unix by syncing the directory; on Windows, where a directory cannot be synced,
/// by syncing the file, which flushes NTFS's journal through its name. A directory's own name there
/// waits for the next sync of a file made in it.
pub fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        sync_dir(dir)?;
    }
    #[cfg(windows)]
    if path.is_file() {
        let file = OpenOptions::new().write(true).open(path)?;
        sync(&file, path)?;
    }
    Ok(())
}

fn remove_tree(path: &Path) -> io::Result<()> {
    std::fs::remove_dir_all(path)
}

#[cfg(test)]
pub(crate) mod fault {
    //! A disk the tests can stop. `Disk::arm` watches a directory: each change `disk` makes under
    //! it is counted and recorded, with what it does to the names there and the data each sync
    //! makes durable. Syncs are not made for real (a crash here is one the tests make). A plan
    //! stops the disk at the nth change (a crash: that change and every later one fail) or fails
    //! that one change (an I/O error, the disk going on). `crash_states` gives what a crash at the
    //! point the disk stopped could leave on disk, and `put` puts one there.

    use super::Op;
    use std::collections::{BTreeMap, BTreeSet};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, MutexGuard};

    type Ino = usize;

    /// What a crash keeps of the names made since their directory's last sync.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Model {
        /// A name once its directory is synced; others in any order, as far as the journal got.
        Posix,
        /// Names in the order made, through the last change of a file synced since; later ones
        /// as far as the journal got.
        Ntfs,
    }

    #[derive(Clone, Copy, Debug)]
    pub enum Plan {
        Watch,
        /// The disk stops at the nth change: it and every later one fail.
        Crash(u64),
        /// The nth change fails; the disk goes on.
        Fail(u64),
    }

    enum Change {
        Link { path: PathBuf, ino: Ino },
        Unlink { path: PathBuf },
        /// Both halves in one journal entry: a crash keeps the one name or the other.
        Rename { from: PathBuf, to: PathBuf, ino: Ino },
        MakeDir { path: PathBuf },
        RemoveDir { path: PathBuf },
    }

    struct State {
        plan: Plan,
        changes: u64,
        /// The change the disk stopped at.
        stopped: Option<String>,
        /// The change a failure plan failed.
        failed: Option<String>,
        start_files: BTreeMap<PathBuf, Ino>,
        start_dirs: BTreeSet<PathBuf>,
        files: BTreeMap<PathBuf, Ino>,
        dirs: BTreeSet<PathBuf>,
        /// Per file, its data as of its last sync.
        synced: Vec<Vec<u8>>,
        /// Per file, the log entries through its last naming (its creation or a rename to it).
        named: Vec<usize>,
        log: Vec<Change>,
        /// Per directory, the log entries before its last sync.
        dir_synced: BTreeMap<PathBuf, usize>,
        /// The log entries a file's sync flushed (NTFS).
        journal: usize,
    }

    impl State {
        fn new_file(&mut self, data: Vec<u8>) -> Ino {
            self.synced.push(data);
            self.named.push(self.log.len());
            self.synced.len() - 1
        }
    }

    pub struct Disk {
        root: PathBuf,
        state: Mutex<State>,
    }

    static DISKS: Mutex<Vec<Arc<Disk>>> = Mutex::new(Vec::new());

    fn disks() -> MutexGuard<'static, Vec<Arc<Disk>>> {
        DISKS.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A disk watching a directory while held.
    pub struct Armed(Arc<Disk>);

    impl Drop for Armed {
        fn drop(&mut self) {
            disks().retain(|d| !Arc::ptr_eq(d, &self.0));
        }
    }

    impl std::ops::Deref for Armed {
        type Target = Disk;
        fn deref(&self) -> &Disk {
            &self.0
        }
    }

    /// The files and directories under `root`, relative to it, with the files' data.
    fn scan(root: &Path) -> (BTreeMap<PathBuf, Vec<u8>>, BTreeSet<PathBuf>) {
        fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>, dirs: &mut BTreeSet<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let path = e.path();
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                if path.is_dir() {
                    dirs.insert(rel);
                    walk(root, &path, files, dirs);
                } else if let Ok(data) = std::fs::read(&path) {
                    files.insert(rel, data);
                }
            }
        }
        let (mut files, mut dirs) = (BTreeMap::new(), BTreeSet::new());
        walk(root, root, &mut files, &mut dirs);
        (files, dirs)
    }

    impl Disk {
        /// Watch `root`: what is under it now is on disk as it is.
        pub fn arm(root: &Path, plan: Plan) -> Armed {
            let (files, dirs) = scan(root);
            let mut state = State {
                plan,
                changes: 0,
                stopped: None,
                failed: None,
                start_files: BTreeMap::new(),
                start_dirs: dirs.clone(),
                files: BTreeMap::new(),
                dirs,
                synced: Vec::new(),
                named: Vec::new(),
                log: Vec::new(),
                dir_synced: BTreeMap::new(),
                journal: 0,
            };
            for (path, data) in files {
                let ino = state.new_file(data);
                state.start_files.insert(path.clone(), ino);
                state.files.insert(path, ino);
            }
            let disk = Arc::new(Disk { root: root.to_path_buf(), state: Mutex::new(state) });
            let mut all = disks();
            assert!(!all.iter().any(|d| d.root.starts_with(root) || root.starts_with(&d.root)), "{}: watched already", root.display());
            all.push(Arc::clone(&disk));
            Armed(disk)
        }

        fn state(&self) -> MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// Changes made so far (those failed included).
        pub fn changes(&self) -> u64 {
            self.state().changes
        }

        /// The change the disk stopped at.
        pub fn stopped(&self) -> Option<String> {
            self.state().stopped.clone()
        }

        /// The change a failure plan failed.
        pub fn failed(&self) -> Option<String> {
            self.state().failed.clone()
        }

        /// What a crash now could leave on disk: what the process wrote (it stopped); and under
        /// `model`, the names it keeps with each later part of the log, the data as synced; and
        /// the data written after a sync torn (cut short, zeros, garbage).
        pub fn crash_states(&self, model: Model, seed: u64) -> Vec<CrashState> {
            let s = self.state();
            let (files, dirs) = scan(&self.root);
            let written = files.clone();
            let mut out = vec![CrashState { label: "the process stopped".into(), files, dirs }];
            // a log entry the model keeps whatever the journal got to
            let kept = |i: usize, path: &Path| match model {
                Model::Posix => s.dir_synced.get(path.parent().unwrap_or(Path::new(""))).is_some_and(|&n| i < n),
                Model::Ntfs => i < s.journal,
            };
            let names = |upto: usize| {
                let mut files = s.start_files.clone();
                let mut dirs = s.start_dirs.clone();
                for (i, c) in s.log.iter().enumerate() {
                    let keep = |p: &Path| i < upto || kept(i, p);
                    match c {
                        Change::Link { path, ino } if keep(path) => {
                            files.insert(path.clone(), *ino);
                        }
                        Change::Unlink { path } if keep(path) => {
                            files.remove(path);
                        }
                        Change::Rename { from, to, ino } => {
                            if keep(to) {
                                files.insert(to.clone(), *ino);
                            }
                            if keep(from) && files.get(from) == Some(ino) {
                                files.remove(from);
                            }
                        }
                        Change::MakeDir { path } if keep(path) => {
                            dirs.insert(path.clone());
                        }
                        Change::RemoveDir { path } if keep(path) => {
                            // what it held went with it: a directory made again of the name is
                            // another, empty
                            dirs.retain(|d| !d.starts_with(path));
                            files.retain(|f, _| !f.starts_with(path));
                        }
                        _ => {}
                    }
                }
                let reachable = |p: &Path, dirs: &BTreeSet<PathBuf>| p.ancestors().skip(1).filter(|a| !a.as_os_str().is_empty()).all(|a| dirs.contains(a));
                let all_dirs = dirs.clone();
                dirs.retain(|d| reachable(d, &all_dirs));
                files.retain(|p, _| reachable(p, &dirs));
                (files, dirs)
            };
            let current: BTreeMap<Ino, &PathBuf> = s.files.iter().map(|(p, &i)| (i, p)).collect();
            let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let mut next = move || {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng
            };
            // the parts of the log that keep something more: through each entry the model does not
            // keep whole anyway
            let whole = |i: usize, c: &Change| match c {
                Change::Rename { from, to, .. } => kept(i, from) && kept(i, to),
                Change::Link { path, .. } | Change::Unlink { path } | Change::MakeDir { path } | Change::RemoveDir { path } => kept(i, path),
            };
            let cuts: Vec<usize> = std::iter::once(0).chain(s.log.iter().enumerate().filter(|(i, c)| !whole(*i, c)).map(|(i, _)| i + 1)).collect();
            for &upto in &cuts {
                let (files, dirs) = names(upto);
                let files = files.into_iter().map(|(p, ino)| (p, s.synced[ino].clone())).collect();
                out.push(CrashState { label: format!("{model:?}, the first {upto} of {} names kept, data as synced", s.log.len()), files, dirs });
            }
            // every name kept, the data written since a sync torn
            let (files, dirs) = names(s.log.len());
            let torn: Vec<(&PathBuf, Ino, usize)> = files
                .iter()
                .filter_map(|(p, &ino)| {
                    let real = written.get(*current.get(&ino)?)?;
                    let synced = &s.synced[ino];
                    (real.len() > synced.len() && real.starts_with(synced)).then_some((p, ino, real.len() - synced.len()))
                })
                .collect();
            if !torn.is_empty() {
                for (label, kind) in [("cut short", 0), ("zeros", 1), ("garbage", 2)] {
                    let mut out_files: BTreeMap<PathBuf, Vec<u8>> = files.iter().map(|(p, &ino)| (p.clone(), s.synced[ino].clone())).collect();
                    for &(p, ino, extra) in &torn {
                        let real = &written[current[&ino]];
                        let data = out_files.get_mut(p).unwrap();
                        let n = 1 + (next() as usize) % extra;
                        match kind {
                            0 => data.extend_from_slice(&real[data.len()..data.len() + n]),
                            1 => data.resize(data.len() + n, 0),
                            _ => data.extend((0..n).map(|_| next() as u8)),
                        }
                    }
                    out.push(CrashState { label: format!("{model:?}, every name kept, data written after a sync {label}"), files: out_files, dirs: dirs.clone() });
                }
            }
            out
        }
    }

    /// What a crash left on disk.
    #[derive(Clone, PartialEq, Eq)]
    pub struct CrashState {
        pub label: String,
        files: BTreeMap<PathBuf, Vec<u8>>,
        dirs: BTreeSet<PathBuf>,
    }

    impl CrashState {
        /// Put it in `root`, in place of what is there.
        pub fn put(&self, root: &Path) {
            for e in std::fs::read_dir(root).unwrap().flatten() {
                let path = e.path();
                let r = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
                r.unwrap_or_else(|e| panic!("{}: {e} (still mapped?)", path.display()));
            }
            for d in &self.dirs {
                std::fs::create_dir_all(root.join(d)).unwrap();
            }
            for (p, data) in &self.files {
                std::fs::write(root.join(p), data).unwrap();
            }
        }

        /// The same files and directories.
        pub fn same(&self, other: &CrashState) -> bool {
            self.files == other.files && self.dirs == other.dirs
        }
    }

    fn describe(op: &Op<'_>, path: &Path, root: &Path) -> String {
        let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
        match op {
            Op::Create => format!("create {}", rel(path)),
            Op::Sync => format!("sync {}", rel(path)),
            Op::Cut => format!("cut {}", rel(path)),
            Op::Rename(to) => format!("rename {} to {}", rel(path), rel(to)),
            Op::Remove => format!("remove {}", rel(path)),
            Op::RemoveTree => format!("remove the tree {}", rel(path)),
            Op::MakeDirs => format!("make the directories {}", rel(path)),
            Op::SyncDir => format!("sync the directory {}", rel(path)),
        }
    }

    enum Kind {
        Create,
        Sync,
        Cut,
        Rename(PathBuf),
        Remove,
        /// What was there, files before the directories holding them.
        RemoveTree(Vec<(PathBuf, bool)>),
        /// The directories missing, outermost first.
        MakeDirs(Vec<PathBuf>),
        SyncDir,
    }

    /// A change under way.
    pub(crate) struct Pending(Option<(Arc<Disk>, Kind, PathBuf)>);

    pub(crate) fn before(op: Op<'_>, path: &Path) -> io::Result<Pending> {
        let Some(disk) = disks().iter().find(|d| path.starts_with(&d.root)).cloned() else { return Ok(Pending(None)) };
        let what = describe(&op, path, &disk.root);
        {
            let mut s = disk.state();
            s.changes += 1;
            if let Some(at) = &s.stopped {
                return Err(io::Error::other(format!("the disk stopped at: {at}")));
            }
            match s.plan {
                Plan::Crash(n) if s.changes == n => {
                    s.stopped = Some(what.clone());
                    return Err(io::Error::other(format!("the disk stopped at: {what}")));
                }
                Plan::Fail(n) if s.changes == n => {
                    s.failed = Some(what.clone());
                    return Err(io::Error::other(format!("an I/O error at: {what}")));
                }
                _ => {}
            }
        }
        let kind = match op {
            Op::Create => Kind::Create,
            Op::Sync => Kind::Sync,
            Op::Cut => Kind::Cut,
            Op::Rename(to) => Kind::Rename(to.to_path_buf()),
            Op::Remove => Kind::Remove,
            Op::RemoveTree => {
                fn walk(dir: &Path, out: &mut Vec<(PathBuf, bool)>) {
                    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            walk(&p, out);
                        } else {
                            out.push((p, false));
                        }
                    }
                    out.push((dir.to_path_buf(), true));
                }
                let mut listed = Vec::new();
                if path.is_dir() {
                    walk(path, &mut listed);
                }
                Kind::RemoveTree(listed)
            }
            Op::MakeDirs => {
                let mut missing: Vec<PathBuf> = path.ancestors().take_while(|a| a.starts_with(&disk.root) && *a != disk.root.as_path() && !a.is_dir()).map(Path::to_path_buf).collect();
                missing.reverse();
                Kind::MakeDirs(missing)
            }
            Op::SyncDir => Kind::SyncDir,
        };
        Ok(Pending(Some((disk, kind, path.to_path_buf()))))
    }

    impl Pending {
        /// Make the change for real: not a sync of a watched disk (the tests make its crashes).
        pub(crate) fn real(&self) -> bool {
            !matches!(&self.0, Some((_, Kind::Sync | Kind::SyncDir, _)))
        }

        pub(crate) fn after(self, ok: bool) {
            let Some((disk, kind, path)) = self.0 else { return };
            let rel = |p: &Path| p.strip_prefix(&disk.root).unwrap().to_path_buf();
            let mut s = disk.state();
            match kind {
                Kind::Create if ok => {
                    let r = rel(&path);
                    // (a file of the name is cut to nothing: its data as synced stays until a sync)
                    if !s.files.contains_key(&r) {
                        let ino = s.new_file(Vec::new());
                        s.files.insert(r.clone(), ino);
                        s.log.push(Change::Link { path: r, ino });
                        s.named[ino] = s.log.len();
                    }
                }
                Kind::Sync if ok => {
                    let ino = *s.files.get(&rel(&path)).unwrap_or_else(|| panic!("{}: synced, not created through persistence::disk", path.display()));
                    s.synced[ino] = std::fs::read(&path).unwrap();
                    s.journal = s.journal.max(s.named[ino]);
                }
                Kind::Rename(to) if ok => {
                    let (from, to) = (rel(&path), rel(&to));
                    let ino = s.files.remove(&from).unwrap_or_else(|| panic!("{}: renamed, not created through persistence::disk", path.display()));
                    s.files.insert(to.clone(), ino);
                    s.log.push(Change::Rename { from, to, ino });
                    s.named[ino] = s.log.len();
                }
                Kind::Remove if ok => {
                    let r = rel(&path);
                    if s.files.remove(&r).is_some() {
                        s.log.push(Change::Unlink { path: r });
                    }
                }
                Kind::RemoveTree(listed) => {
                    // (what went, the removal cut short or not)
                    for (p, is_dir) in listed.into_iter().filter(|(p, _)| !p.exists()) {
                        let r = rel(&p);
                        if is_dir {
                            if s.dirs.remove(&r) {
                                s.log.push(Change::RemoveDir { path: r });
                            }
                        } else if s.files.remove(&r).is_some() {
                            s.log.push(Change::Unlink { path: r });
                        }
                    }
                }
                Kind::MakeDirs(missing) => {
                    for p in missing.into_iter().filter(|p| p.is_dir()) {
                        let r = rel(&p);
                        if s.dirs.insert(r.clone()) {
                            s.log.push(Change::MakeDir { path: r });
                        }
                    }
                }
                Kind::SyncDir if ok => {
                    let n = s.log.len();
                    s.dir_synced.insert(rel(&path), n);
                }
                _ => {}
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::persistence::disk;
        use std::io::Write;

        /// The power-loss state that keeps only what the model must: no part of the log beyond.
        fn minimal(disk: &Disk, model: Model) -> CrashState {
            disk.crash_states(model, 1).into_iter().find(|s| s.label.contains("the first 0 of")).unwrap()
        }

        fn write_file(path: &Path, data: &[u8]) {
            let mut f = disk::create(path).unwrap();
            f.write_all(data).unwrap();
            disk::sync(&f, path).unwrap();
        }

        /// A file renamed over another is kept once its name is made durable: by syncing the
        /// directory (POSIX), or a sync of the file flushing the journal through its name (NTFS).
        #[test]
        fn a_rename_is_kept_once_its_name_is_durable() {
            let dir = tempfile::tempdir().unwrap();
            let (old, tmp) = (dir.path().join("file"), dir.path().join("file.tmp"));
            std::fs::write(&old, b"1").unwrap();
            let disk = Disk::arm(dir.path(), Plan::Watch);
            write_file(&tmp, b"2");
            disk::rename(&tmp, &old).unwrap();
            let models: &[Model] = if cfg!(windows) { &[Model::Posix, Model::Ntfs] } else { &[Model::Posix] };
            for &model in models {
                let st = minimal(&disk, model);
                assert_eq!(st.files[Path::new("file")], b"1", "{model:?}: the rename is not durable yet");
                // (on NTFS the temporary file's sync flushed its name: a stray the next start removes)
                assert_eq!(st.files.contains_key(Path::new("file.tmp")), model == Model::Ntfs, "{model:?}");
            }
            disk::sync_parent(&old).unwrap();
            for &model in models {
                let st = minimal(&disk, model);
                assert_eq!(st.files[Path::new("file")], b"2", "{model:?}");
                assert_eq!(st.files.len(), 1);
            }
            // the process stopping keeps what was written either way
            assert_eq!(disk.crash_states(Model::Posix, 1)[0].files[Path::new("file")], b"2");
        }

        /// POSIX keeps a name made in a synced directory whatever became of other directories.
        #[test]
        fn posix_keeps_the_names_of_a_synced_directory_only() {
            let dir = tempfile::tempdir().unwrap();
            let disk = Disk::arm(dir.path(), Plan::Watch);
            let (a, b) = (dir.path().join("a"), dir.path().join("b"));
            disk::create_dirs(&a).unwrap();
            disk::create_dirs(&b).unwrap();
            write_file(&a.join("x"), b"x");
            write_file(&b.join("y"), b"y");
            disk::sync_dir(&a).unwrap();
            let st = minimal(&disk, Model::Posix);
            assert!(st.files.contains_key(&Path::new("a").join("x")));
            assert!(!st.files.contains_key(&Path::new("b").join("y")));
            // each later part of the journal is a state of its own: the last keeps every name
            let all = disk.crash_states(Model::Posix, 1);
            assert!(all.iter().any(|s| s.files.contains_key(&Path::new("b").join("y"))));
        }

        /// Data appended after a sync: a power loss keeps what the sync made durable; torn states
        /// add part of the rest, cut short, zeros or garbage.
        #[test]
        fn data_after_a_sync_is_lost_or_torn() {
            let dir = tempfile::tempdir().unwrap();
            let disk = Disk::arm(dir.path(), Plan::Watch);
            let log = dir.path().join("log");
            let mut f = disk::create_new_append(&log).unwrap();
            disk::sync_parent(&log).unwrap();
            f.write_all(b"abc").unwrap();
            disk::sync_data(&f, &log).unwrap();
            f.write_all(b"defgh").unwrap();
            let states = disk.crash_states(Model::Posix, 7);
            assert_eq!(states[0].files[Path::new("log")], b"abcdefgh", "the process stopped");
            assert_eq!(minimal(&disk, Model::Posix).files[Path::new("log")], b"abc");
            let torn: Vec<&CrashState> = states.iter().filter(|s| s.label.contains("after a sync")).collect();
            assert_eq!(torn.len(), 3);
            for st in torn {
                let data = &st.files[Path::new("log")];
                assert!(data.starts_with(b"abc") && data.len() > 3 && data.len() <= 8, "{}: {data:?}", st.label);
                if st.label.contains("cut short") {
                    assert!(b"abcdefgh".starts_with(data));
                }
                if st.label.contains("zeros") {
                    assert!(data[3..].iter().all(|&b| b == 0));
                }
            }
        }

        /// A directory removed for good takes what it held: one made again under the name is
        /// another, empty, whatever of the first was not durable.
        #[test]
        fn a_directory_made_again_holds_nothing_of_the_one_removed() {
            let dir = tempfile::tempdir().unwrap();
            let disk = Disk::arm(dir.path(), Plan::Watch);
            let sub = dir.path().join("sub");
            disk::create_dirs(&sub).unwrap();
            write_file(&sub.join("x"), b"x");
            disk::sync_dir(&sub).unwrap();
            disk::remove_dir_all(&sub).unwrap();
            disk::sync_parent(&sub).unwrap();
            disk::create_dirs(&sub).unwrap();
            let st = minimal(&disk, Model::Posix);
            assert!(st.dirs.contains(Path::new("sub")));
            assert!(st.files.is_empty(), "{:?}", st.files.keys().collect::<Vec<_>>());
        }

        /// A crash at a change: it fails, and every later one; the disk says where it stopped.
        #[test]
        fn a_crash_stops_every_change_from_it() {
            let dir = tempfile::tempdir().unwrap();
            let disk = Disk::arm(dir.path(), Plan::Crash(2));
            let a = disk::create(&dir.path().join("a")).unwrap();
            assert!(disk::create(&dir.path().join("b")).is_err());
            assert!(disk::sync(&a, &dir.path().join("a")).is_err());
            assert_eq!(disk.stopped().as_deref(), Some("create b"));
            assert_eq!(disk.changes(), 3);
        }

        /// A failure fails that one change; the disk goes on.
        #[test]
        fn a_failure_fails_one_change() {
            let dir = tempfile::tempdir().unwrap();
            let disk = Disk::arm(dir.path(), Plan::Fail(1));
            assert!(disk::create(&dir.path().join("a")).is_err());
            assert!(disk::create(&dir.path().join("b")).is_ok());
            assert!(disk.stopped().is_none());
        }
    }
}
