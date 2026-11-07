//! C ABI over `torque::build::SegmentBuilder`, for the client SDKs (the .NET client's
//! `TqbfSegmentBuilder`). A builder is shared by the caller's threads; a run belongs to one
//! thread at a time. Functions return 0 (or a non-null pointer) on success; on failure the
//! message is left for `tqb_last_error` on the calling thread. Panics are caught at the boundary.
//! Byte buffers handed out (`tqb_run_end`, `tqb_finish`) are released with `tqb_bytes_free`.
//!
//! Memory comes from the system allocator, not mimalloc as in the server: the library is a guest in
//! the caller's process and runs on the caller's threads, and a build gives its memory back when it
//! is freed: under mimalloc a finished build stayed committed into the next one in the process.
//! Builds are as fast either way.

use std::cell::RefCell;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use torque::build::{BuildOptions, BuildReport, Rejected, RunBuilder, SegmentBuilder};

thread_local! {
    static LAST_ERROR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn fail(msg: String) {
    LAST_ERROR.with(|e| *e.borrow_mut() = msg.into_bytes());
}

/// Run `f`, turning an error or a panic into `on_fail` and the thread's last error.
fn guard<T>(on_fail: T, f: impl FnOnce() -> io::Result<T>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            fail(e.to_string());
            on_fail
        }
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".to_string());
            fail(format!("torque_build panicked: {msg}"));
            on_fail
        }
    }
}

unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len) } }
}

fn utf8(b: &[u8]) -> io::Result<&str> {
    std::str::from_utf8(b).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

unsafe fn hand_out(v: Vec<u8>, out: *mut *mut u8, out_len: *mut usize) {
    let b = v.into_boxed_slice();
    unsafe {
        *out_len = b.len();
        *out = Box::into_raw(b) as *mut u8;
    }
}

fn rejected_json(r: &[Rejected]) -> serde_json::Value {
    serde_json::Value::Array(
        r.iter().map(|r| serde_json::json!({"run": r.run, "index": r.index, "id": r.id, "error": r.error})).collect(),
    )
}

fn report_json(r: &BuildReport) -> serde_json::Value {
    serde_json::json!({
        "docs": r.docs,
        "pieces": r.pieces,
        "bytes": r.bytes,
        "frozen_bytes": r.frozen_bytes,
        "merge_millis": r.merge_millis as u64,
        "rejected": rejected_json(&r.rejected),
    })
}

#[repr(C)]
pub struct TqbOptions {
    /// zstd level of the stored documents.
    pub docs_level: i32,
    /// Threads `tqb_finish` merges with (0 = all cores).
    pub threads: u32,
    /// Accumulator bytes after which a run freezes a piece (0 = default).
    pub piece_bytes: u64,
    /// 0: frozen pieces stay in memory; else they go to a spill file in `spill_dir` (UTF-8;
    /// null = the system temp directory).
    pub spill: u32,
    pub spill_dir: *const u8,
    pub spill_dir_len: usize,
}

/// A builder for one segment of the collection `schema` (UTF-8 JSON, as `POST /collections`
/// takes it). Null on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tqb_new(schema: *const u8, schema_len: usize, opts: *const TqbOptions) -> *mut SegmentBuilder {
    guard(std::ptr::null_mut(), || {
        let mut o = BuildOptions::default();
        if let Some(opts) = unsafe { opts.as_ref() } {
            o.docs_level = opts.docs_level;
            o.threads = opts.threads as usize;
            if opts.piece_bytes > 0 {
                o.piece_bytes = opts.piece_bytes as usize;
            }
            o.spill_dir = match opts.spill {
                0 => None,
                _ if opts.spill_dir.is_null() => Some(std::env::temp_dir()),
                _ => Some(std::path::PathBuf::from(utf8(unsafe { bytes(opts.spill_dir, opts.spill_dir_len) })?)),
            };
        }
        let b = SegmentBuilder::new(unsafe { bytes(schema, schema_len) }, o)?;
        Ok(Box::into_raw(Box::new(b)))
    })
}

/// Drop a builder that will not be finished.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tqb_free(b: *mut SegmentBuilder) {
    if !b.is_null() {
        drop(unsafe { Box::from_raw(b) });
    }
}

/// Registry id of key `name` (UTF-8) under `parent` (0xFFFF = top level), added when new.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tqb_path_id(b: *const SegmentBuilder, parent: u16, name: *const u8, name_len: usize, out: *mut u16) -> i32 {
    guard(-1, || {
        let b = unsafe { b.as_ref() }.ok_or_else(|| io::Error::other("null builder"))?;
        let id = b.path_id(parent, utf8(unsafe { bytes(name, name_len) })?)?;
        unsafe { *out = id };
        Ok(0)
    })
}

/// Start run `key` (runs are laid out in key order). Null on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tqb_run_begin(b: *const SegmentBuilder, key: u64) -> *mut RunBuilder {
    guard(std::ptr::null_mut(), || {
        let b = unsafe { b.as_ref() }.ok_or_else(|| io::Error::other("null builder"))?;
        Ok(Box::into_raw(Box::new(b.run(key))))
    })
}

/// Add records, each `varint(len) record`. Refused documents are reported by `tqb_run_end`;
/// an error means an unusable record or a document id the run already has.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tqb_run_add(r: *mut RunBuilder, records: *const u8, len: usize) -> i32 {
    guard(-1, || {
        let r = unsafe { r.as_mut() }.ok_or_else(|| io::Error::other("null run"))?;
        r.add_batch(unsafe { bytes(records, len) })?;
        Ok(0)
    })
}

/// Number of documents the run refused so far.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tqb_run_rejected(r: *const RunBuilder) -> u64 {
    unsafe { r.as_ref() }.map_or(0, |r| r.rejected().len() as u64)
}

/// End the run (it is freed, even on error). `out` receives the refused documents as a UTF-8
/// JSON array of {run, index, id, error}.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tqb_run_end(r: *mut RunBuilder, out: *mut *mut u8, out_len: *mut usize) -> i32 {
    guard(-1, || {
        if r.is_null() {
            return Err(io::Error::other("null run"));
        }
        let run = unsafe { Box::from_raw(r) };
        let rejected = run.end()?;
        unsafe { hand_out(serde_json::to_vec(&rejected_json(&rejected))?, out, out_len) };
        Ok(0)
    })
}

/// Drop a run that will not be ended (its documents are left out of the segment).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tqb_run_free(r: *mut RunBuilder) {
    if !r.is_null() {
        drop(unsafe { Box::from_raw(r) });
    }
}

/// Merge the ended runs and write the segment file to `path` (UTF-8). The builder is freed, even
/// on error. `out` receives the report as UTF-8 JSON.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tqb_finish(b: *mut SegmentBuilder, path: *const u8, path_len: usize, out: *mut *mut u8, out_len: *mut usize) -> i32 {
    guard(-1, || {
        if b.is_null() {
            return Err(io::Error::other("null builder"));
        }
        let b = unsafe { Box::from_raw(b) };
        let path = utf8(unsafe { bytes(path, path_len) })?.to_string();
        let report = b.finish(std::path::Path::new(&path))?;
        unsafe { hand_out(serde_json::to_vec(&report_json(&report))?, out, out_len) };
        Ok(0)
    })
}

/// Release a buffer handed out by `tqb_run_end` or `tqb_finish`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tqb_bytes_free(p: *mut u8, len: usize) {
    if !p.is_null() {
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(p, len)) });
    }
}

/// Copy the calling thread's last error (UTF-8) into `buf`; returns its full length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tqb_last_error(buf: *mut u8, cap: usize) -> usize {
    LAST_ERROR.with(|e| {
        let e = e.borrow();
        if !buf.is_null() {
            let n = e.len().min(cap);
            unsafe { std::ptr::copy_nonoverlapping(e.as_ptr(), buf, n) };
        }
        e.len()
    })
}
