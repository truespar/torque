//! Segment upload (spec §10.3): `PUT /collections/{name}/segments?part=i&parts=n` streams one file
//! per request into a staging directory: a segment file built by `torque-admin build`, or a
//! docs-only file the server builds itself. Transport codecs are removed and every checksum is
//! verified as a part arrives. The request that completes the set creates the collection from the
//! files' SCHEMA, installs the segments as its first generation and registers it; nothing is
//! decoded or indexed for a built segment. Readers move over with `PUT /aliases/{alias}`.

use super::error::AppError;
use super::response::CollectionResponse;
use super::AppState;
use crate::collection::CollectionError;
use crate::ingest::segment_file::{self, BuildOptions};
use crate::persistence;
use crate::tqbf::container::{kind, remove_transport_codecs, SegmentFile, SEGMENT_WIDE};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::put;
use axum::Router;
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Upload routes. Mounted outside the request timeout: a part can take minutes to arrive.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/collections/{name}/segments", put(upload_segment))
}

fn default_parts() -> u32 {
    1
}

#[derive(Deserialize)]
struct UploadParams {
    #[serde(default)]
    part: u32,
    #[serde(default = "default_parts")]
    parts: u32,
}

fn io_err(e: std::io::Error) -> AppError {
    match e.kind() {
        std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => AppError::BadRequest(e.to_string()),
        _ => AppError::Internal(e.to_string()),
    }
}

fn staging_dir(state: &AppState, name: &str) -> PathBuf {
    state.config.data_dir.join("uploads").join(name)
}

fn part_path(dir: &std::path::Path, part: u32) -> PathBuf {
    dir.join(format!("part_{part:08}.tqbf"))
}

/// One commit at a time, so two parts completing together cannot both install the set.
fn commit_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn upload_segment(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(params): Query<UploadParams>,
    body: Body,
) -> Result<Response, AppError> {
    if params.parts == 0 || params.parts > 4096 || params.part >= params.parts {
        return Err(AppError::BadRequest(format!("part {} of {} is not a valid part", params.part, params.parts)));
    }
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err(AppError::BadRequest(format!("`{name}` is not a valid collection name")));
    }
    if state.aliases.get(&name).is_some() {
        return Err(AppError::Conflict(format!(
            "An alias with name `{name}` already exists — collection names cannot shadow alias names."
        )));
    }
    if state.collections.get(&name).is_some() {
        return Err(AppError::Conflict(format!("Collection `{name}` already exists.")));
    }
    crate::memory::check_disk_preflight().map_err(AppError::ServiceUnavailable)?;

    // Stream the body to disk: parts are far larger than anything held in memory.
    let start = Instant::now();
    let dir = staging_dir(&state, &name);
    tokio::fs::create_dir_all(&dir).await.map_err(io_err)?;
    let final_path = part_path(&dir, params.part);
    let tmp = crate::tqbf::container::unique_tmp_path(&final_path);
    let received = {
        use tokio_stream::StreamExt;
        use tokio::io::AsyncWriteExt;
        let mut f = tokio::fs::File::create(&tmp).await.map_err(io_err)?;
        let mut stream = body.into_data_stream();
        let mut n = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| AppError::BadRequest(format!("upload interrupted: {e}")))?;
            n += chunk.len() as u64;
            f.write_all(&chunk).await.map_err(io_err)?;
        }
        f.sync_all().await.map_err(io_err)?;
        n
    };

    // Remove transport codecs and verify every section before the part counts as staged.
    let (tmp2, final2) = (tmp.clone(), final_path.clone());
    let doc_count = tokio::task::spawn_blocking(move || -> std::io::Result<u32> {
        let file = SegmentFile::open_transport(&tmp2)?;
        let coded = file.has_transport_codecs();
        drop(file);
        if coded {
            remove_transport_codecs(&tmp2, &final2)?;
            std::fs::remove_file(&tmp2).ok();
        } else {
            std::fs::rename(&tmp2, &final2)?;
        }
        let file = SegmentFile::open(&final2)?;
        file.verify()?;
        if file.section(kind::SCHEMA, SEGMENT_WIDE).is_none() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "file has no SCHEMA section"));
        }
        Ok(file.doc_count)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    let doc_count = match doc_count {
        Ok(n) => n,
        Err(e) => {
            std::fs::remove_file(&tmp).ok();
            std::fs::remove_file(&final_path).ok();
            return Err(io_err(e));
        }
    };
    tracing::info!(collection = %name, part = params.part, parts = params.parts, docs = doc_count, bytes = received, "segment part staged");

    let _commit = commit_lock().lock().await;
    let files: Vec<PathBuf> = (0..params.parts).map(|i| part_path(&dir, i)).collect();
    if !files.iter().all(|f| f.exists()) {
        let body = json!({"staged": params.part, "parts": params.parts, "num_documents": doc_count});
        return Ok((StatusCode::ACCEPTED, Json(body)).into_response());
    }
    let collection = commit(&state, &name, dir, files).await?;
    tracing::info!(
        collection = %name,
        docs = collection.index.load().total_docs(),
        ms = start.elapsed().as_millis() as u64,
        "uploaded segments installed"
    );
    Ok(Json(CollectionResponse::from_collection(&collection)).into_response())
}

/// Create the collection from a complete set of staged parts.
async fn commit(
    state: &Arc<AppState>,
    name: &str,
    dir: PathBuf,
    files: Vec<PathBuf>,
) -> Result<Arc<crate::collection::Collection>, AppError> {
    let staging = dir.clone();
    let prepared = tokio::task::spawn_blocking(move || -> std::io::Result<(crate::schema::Schema, Vec<PathBuf>)> {
        let mut schema_bytes: Option<Vec<u8>> = None;
        let mut schema = None;
        let mut out = Vec::with_capacity(files.len());
        for (i, f) in files.iter().enumerate() {
            let file = SegmentFile::open(f)?;
            let bytes = file.section(kind::SCHEMA, SEGMENT_WIDE).map(|b| b.to_vec()).unwrap_or_default();
            match &schema_bytes {
                None => {
                    schema = Some(segment_file::file_schema(&file)?);
                    schema_bytes = Some(bytes);
                }
                Some(first) if *first != bytes => {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("part {i} has a different SCHEMA than part 0")));
                }
                Some(_) => {}
            }
            if segment_file::is_docs_only(&file) {
                // Separately written docs-only files number their paths independently.
                if files.len() > 1 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "a docs-only file is uploaded as a single part (split collections are built with torque-admin build)",
                    ));
                }
                drop(file);
                // A docs-only part becomes one segment, built here at the server's zstd level.
                let opts = BuildOptions { docs_level: 3 };
                let report = segment_file::build(f, &staging.join(format!("built_{i:08}")), &opts)?;
                if !report.rejected.is_empty() {
                    let r = &report.rejected[0];
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("part {i}: {} documents rejected, the first ({}, id `{}`): {}", report.rejected.len(), r.index, r.id, r.error),
                    ));
                }
                out.extend(report.segments.into_iter().map(|(p, _)| p));
            } else {
                out.push(f.clone());
            }
        }
        let schema = schema.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no parts"))?;
        Ok((schema, out))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
    .map_err(io_err)?;
    let (mut schema, segment_files) = prepared;

    schema.name = name.to_string();
    let _fs_guard = state.fs_serial.lock().await;
    // the segments moved into the collection's set, then collections.json: nothing of it stays on an error
    let (data_dir, collections) = (state.config.data_dir.clone(), Arc::clone(&state.collections));
    let collection = tokio::task::spawn_blocking(move || persistence::install_collection(&data_dir, &collections, schema, &segment_files))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| match e {
            CollectionError::AlreadyExists(n) => AppError::Conflict(format!("Collection `{n}` already exists.")),
            CollectionError::Storage(msg) => AppError::Internal(msg),
            other => AppError::BadRequest(other.to_string()),
        })?;
    persistence::disk::remove_dir_all(&dir).ok();
    Ok(collection)
}
