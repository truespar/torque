use crate::collection::{Collection, CollectionRegistry, WriteError};
#[cfg(not(feature = "cuda"))]
use crate::ingest::build_index;
use crate::ingest::protocol::*;
use crate::memory::MemoryGuard;
use crate::observability::{CollectionLabels, IngestLabels, MetricsCollector};
use crate::schema::{Document, IndexMode};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Maximum documents per ingest session. Prevents unbounded memory growth
/// from a single client streaming too many docs before committing.
const MAX_DOCS_PER_SESSION: u64 = 50_000_000;

#[derive(Debug, PartialEq, Eq)]
enum SessionState {
    Idle,
    Streaming,
}

pub struct IngestSession {
    collection: Option<Arc<Collection>>,
    state: SessionState,
    documents: Vec<Document>,
    batch_seq: u64,
    docs_received: u64,
    metrics: Arc<MetricsCollector>,
    memory_guard: Option<MemoryGuard>,
    data_dir: Option<PathBuf>,
    #[cfg(feature = "cuda")]
    gpu_context: Option<Arc<crate::gpu::GpuContext>>,
}

impl IngestSession {
    /// Returns true if the session has an active ingest (Streaming).
    pub fn is_active(&self) -> bool {
        self.state != SessionState::Idle
    }

    pub fn new(metrics: Arc<MetricsCollector>) -> Self {
        Self {
            collection: None,
            state: SessionState::Idle,
            documents: Vec::new(),
            batch_seq: 0,
            docs_received: 0,
            metrics,
            memory_guard: None,
            data_dir: None,
            #[cfg(feature = "cuda")]
            gpu_context: None,
        }
    }

    pub fn with_memory_guard(mut self, guard: MemoryGuard) -> Self {
        self.memory_guard = Some(guard);
        self
    }

    pub fn with_data_dir(mut self, data_dir: PathBuf) -> Self {
        self.data_dir = Some(data_dir);
        self
    }

    #[cfg(feature = "cuda")]
    pub fn with_gpu(
        metrics: Arc<MetricsCollector>,
        gpu_context: Option<Arc<crate::gpu::GpuContext>>,
    ) -> Self {
        Self {
            collection: None,
            state: SessionState::Idle,
            documents: Vec::new(),
            batch_seq: 0,
            docs_received: 0,
            metrics,
            memory_guard: None,
            data_dir: None,
            gpu_context,
        }
    }

    pub async fn handle_message(
        &mut self,
        data: &[u8],
        registry: &CollectionRegistry,
    ) -> ServerMessage {
        let msg = match deserialize_client_message(data) {
            Ok(m) => m,
            Err(e) => {
                if let Some(ref col) = self.collection {
                    self.metrics
                        .ingest_parse_errors_total
                        .get_or_create(&CollectionLabels {
                            collection: col.name.clone(),
                        })
                        .inc();
                }
                return ServerMessage::Error {
                    code: ERR_INTERNAL,
                    message: format!("Failed to parse message: {e}"),
                };
            }
        };

        match msg {
            ClientMessage::StartIngest { collection, mode } => {
                self.handle_start_ingest(&collection, mode, registry)
            }
            ClientMessage::DocBatch { doc_count, payload } => {
                self.handle_doc_batch(doc_count, &payload)
            }
            ClientMessage::Commit => self.handle_commit(),
            ClientMessage::Abort => self.handle_abort(),
            ClientMessage::Upsert {
                collection,
                action,
                doc_count,
                payload,
            } => self.handle_upsert(&collection, action, doc_count, &payload, registry).await,
        }
    }

    fn handle_start_ingest(
        &mut self,
        collection_name: &str,
        _mode: u8,
        registry: &CollectionRegistry,
    ) -> ServerMessage {
        if self.state != SessionState::Idle {
            return ServerMessage::Error {
                code: ERR_SESSION_ALREADY_ACTIVE,
                message: "Session already active. Send Abort first.".into(),
            };
        }

        let collection = match registry.get(collection_name) {
            Some(c) => c,
            None => {
                return ServerMessage::Error {
                    code: ERR_COLLECTION_NOT_FOUND,
                    message: format!("Collection `{collection_name}` not found."),
                };
            }
        };

        self.collection = Some(collection);
        self.state = SessionState::Streaming;
        self.documents.clear();
        self.batch_seq = 0;
        self.docs_received = 0;

        ServerMessage::Ack {
            batch_seq: 0,
            docs_received: 0,
        }
    }

    fn handle_doc_batch(&mut self, doc_count: u32, payload: &[u8]) -> ServerMessage {
        if self.state != SessionState::Streaming {
            return ServerMessage::Error {
                code: ERR_NO_ACTIVE_SESSION,
                message: "No active ingest session. Send StartIngest first.".into(),
            };
        }

        let collection = self.collection.as_ref().unwrap();

        // Guard: reject batch early if memory limit is already exceeded
        if let Some(ref guard) = self.memory_guard {
            if let Err(msg) = guard.check() {
                return ServerMessage::Error {
                    code: ERR_LIMIT_EXCEEDED,
                    message: msg,
                };
            }
            // Preflight: estimate if this batch would push us over the limit
            if let Err(msg) = guard.check_preflight(payload.len() as u64) {
                return ServerMessage::Error {
                    code: ERR_LIMIT_EXCEEDED,
                    message: msg,
                };
            }
        }

        // Admission control against currently-available physical memory:
        // a batch doesn't start when the machine can't fit the build. Uses a
        // 4× build multiplier for per-batch work (lower than full-import 6×
        // because the session accumulates docs incrementally rather than all
        // at once).
        if let Err(msg) = crate::memory::check_import_admission_with_factor(
            payload.len() as u64, 4,
        ) {
            return ServerMessage::Error {
                code: ERR_LIMIT_EXCEEDED,
                message: msg,
            };
        }

        // Guard: reject batch if it would exceed session document limit
        let projected = self.docs_received + doc_count as u64;
        if projected > MAX_DOCS_PER_SESSION {
            tracing::warn!(
                collection = %collection.name,
                current = self.docs_received,
                batch = doc_count,
                limit = MAX_DOCS_PER_SESSION,
                "Ingest session document limit exceeded"
            );
            return ServerMessage::Error {
                code: ERR_LIMIT_EXCEEDED,
                message: format!(
                    "Session would contain {projected} documents, exceeding limit of {MAX_DOCS_PER_SESSION}. Commit current batch and start a new session."
                ),
            };
        }

        let start_id = self.documents.len() as u32;
        let decode_start = Instant::now();

        match decode_doc_batch(payload, doc_count, &collection.schema, start_id) {
            Ok(mut docs) => {
                let decode_ms = decode_start.elapsed().as_secs_f64() * 1000.0;
                let count = docs.len() as u64;
                self.documents.append(&mut docs);
                self.batch_seq += 1;
                self.docs_received += count;

                tracing::debug!(
                    collection = %collection.name,
                    batch_seq = self.batch_seq,
                    doc_count = count,
                    decode_ms = format!("{decode_ms:.2}").as_str(),
                    total_docs_received = self.docs_received,
                    payload_bytes = payload.len(),
                    "batch received"
                );

                ServerMessage::Ack {
                    batch_seq: self.batch_seq,
                    docs_received: self.docs_received,
                }
            }
            Err(e) => {
                tracing::warn!(
                    collection = %collection.name,
                    batch_seq = self.batch_seq + 1,
                    doc_count,
                    error = %e,
                    "batch decode failed"
                );
                self.metrics
                    .ingest_schema_mismatches_total
                    .get_or_create(&CollectionLabels {
                        collection: collection.name.clone(),
                    })
                    .inc();
                ServerMessage::Error {
                    code: ERR_SCHEMA_MISMATCH,
                    message: format!("Failed to decode batch: {e}"),
                }
            }
        }
    }

    fn handle_commit(&mut self) -> ServerMessage {
        if self.state != SessionState::Streaming {
            return ServerMessage::Error {
                code: ERR_NO_ACTIVE_SESSION,
                message: "No active ingest session. Send StartIngest first.".into(),
            };
        }

        // Check memory limit before building index
        if let Some(ref guard) = self.memory_guard
            && let Err(msg) = guard.check()
        {
            return ServerMessage::Error {
                code: ERR_LIMIT_EXCEEDED,
                message: msg,
            };
        }

        let collection = self.collection.as_ref().unwrap().clone();
        let documents = std::mem::take(&mut self.documents);
        let doc_count = documents.len() as u64;
        let metrics = self.metrics.clone();

        // Capture generation at commit time. The async build will only swap if
        // this generation is still current, preventing stale builds from overwriting newer ones.
        let build_generation = collection.next_generation();
        let schema = collection.schema.clone();
        let data_dir = self.data_dir.clone();
        #[cfg(feature = "cuda")]
        let gpu = self.gpu_context.clone();

        // Fire-and-forget: spawn the index build in the background.
        // Client gets an immediate ack and polls GET /collections/{name} to check num_documents.
        let build_guard =
            crate::observability::track_in_flight(&metrics.index_builds_in_flight);
        tokio::spawn(async move {
            // Covers build + swap + the inline save below, so quiesce
            // waits for the whole chain, not just the session lifetime.
            let _build_guard = build_guard;
            let start = Instant::now();

            let build_schema = schema.clone();
            let build_collection_name = collection.name.clone();
            let build_data_dir = data_dir.clone();
            let build_result =
                tokio::task::spawn_blocking(move || {
                    #[cfg(feature = "cuda")]
                    {
                        if let Some(ref dir) = build_data_dir {
                            crate::ingest::build_index_with_gpu_and_backing(
                                &build_schema, documents, gpu.as_ref(),
                                dir, &build_collection_name,
                            )
                        } else {
                            crate::ingest::build_index_with_gpu(&build_schema, documents, gpu.as_ref())
                        }
                    }
                    #[cfg(not(feature = "cuda"))]
                    {
                        if let Some(ref dir) = build_data_dir {
                            crate::ingest::build_index_with_backing(
                                &build_schema, documents,
                                dir, &build_collection_name,
                            )
                        } else {
                            build_index(&build_schema, documents)
                        }
                    }
                })
                    .await;

            match build_result {
                Ok(new_index) => {
                    if !collection.swap_index_if_generation(new_index, build_generation) {
                        tracing::warn!(
                            collection = %collection.name,
                            build_generation,
                            "Discarding stale index build (newer commit superseded)"
                        );
                        return;
                    }
                    let elapsed = start.elapsed();

                    metrics
                        .ingest_sessions_total
                        .get_or_create(&IngestLabels {
                            collection: collection.name.clone(),
                            status: "success".into(),
                        })
                        .inc();
                    metrics
                        .ingest_duration
                        .get_or_create(&CollectionLabels {
                            collection: collection.name.clone(),
                        })
                        .observe(elapsed.as_secs_f64());
                    metrics
                        .ingest_docs_total
                        .get_or_create(&CollectionLabels {
                            collection: collection.name.clone(),
                        })
                        .inc_by(doc_count);

                    tracing::info!(
                        collection = %collection.name,
                        docs = doc_count,
                        build_ms = elapsed.as_millis() as u64,
                        "Ingest committed (async)"
                    );

                    // Persist index to disk in the background
                    if let Some(ref dir) = data_dir
                        && schema.persistence_enabled
                        && let Err(e) = collection.save(dir)
                    {
                        tracing::error!(
                            collection = %collection.name,
                            error = %e,
                            "Failed to persist index to disk"
                        );
                    }
                }
                Err(e) => {
                    metrics
                        .ingest_sessions_total
                        .get_or_create(&IngestLabels {
                            collection: collection.name.clone(),
                            status: "error".into(),
                        })
                        .inc();
                    tracing::error!(
                        collection = %collection.name,
                        error = %e,
                        "Index build task failed"
                    );
                }
            }
            crate::memory::purge_allocator();
        });

        // Reset session immediately — client doesn't wait for the build
        self.state = SessionState::Idle;
        self.collection = None;
        self.batch_seq = 0;
        self.docs_received = 0;

        ServerMessage::Ack {
            batch_seq: 0,
            docs_received: doc_count,
        }
    }

    async fn handle_upsert(
        &mut self,
        collection_name: &str,
        action: u8,
        doc_count: u32,
        payload: &[u8],
        registry: &CollectionRegistry,
    ) -> ServerMessage {
        let collection = match registry.get(collection_name) {
            Some(c) => c,
            None => {
                return ServerMessage::Error {
                    code: ERR_COLLECTION_NOT_FOUND,
                    message: format!("Collection `{collection_name}` not found."),
                };
            }
        };

        if collection.schema.index_mode != IndexMode::Realtime {
            return ServerMessage::Error {
                code: ERR_SCHEMA_MISMATCH,
                message: format!(
                    "Collection `{collection_name}` is in batch mode. Use StartIngest/DocBatch/Commit flow."
                ),
            };
        }

        // Check backpressure
        if collection.memory_full() {
            self.metrics.backpressure_events_total.inc();
            self.flush_when_due(&collection);
            return ServerMessage::Backpressure {
                retry_after_ms: 1000,
            };
        }

        match action {
            0 => {
                // Upsert: decode docs, write them
                match decode_doc_batch(payload, doc_count, &collection.schema, 0) {
                    Ok(docs) => match collection.apply_writes(docs, &[]) {
                        Ok(commit) => {
                            self.flush_when_due(&collection);
                            acknowledge(commit, doc_count).await
                        }
                        Err(e) => write_error(e),
                    },
                    Err(e) => {
                        self.metrics
                            .ingest_schema_mismatches_total
                            .get_or_create(&CollectionLabels {
                                collection: collection_name.to_string(),
                            })
                            .inc();
                        ServerMessage::Error {
                            code: ERR_SCHEMA_MISMATCH,
                            message: format!("Failed to decode upsert batch: {e}"),
                        }
                    }
                }
            }
            1 => {
                // Delete: decode external IDs, delete them
                match decode_delete_ids(payload, doc_count) {
                    Ok(ids) => match collection.apply_writes(Vec::new(), &ids) {
                        Ok(commit) => {
                            self.flush_when_due(&collection);
                            acknowledge(commit, doc_count).await
                        }
                        Err(e) => write_error(e),
                    },
                    Err(e) => ServerMessage::Error {
                        code: ERR_INTERNAL,
                        message: format!("Failed to decode delete IDs: {e}"),
                    },
                }
            }
            _ => ServerMessage::Error {
                code: ERR_INTERNAL,
                message: format!("Unknown upsert action: {action}"),
            },
        }
    }

    /// In the background, merge the collection's segments in memory once a run is due and flush
    /// them once they hold enough documents, as the HTTP write paths do.
    fn flush_when_due(&self, collection: &Arc<Collection>) {
        crate::collection::compaction::maintain(collection, self.data_dir.clone(), self.metrics.clone());
    }

    fn handle_abort(&mut self) -> ServerMessage {
        if self.state != SessionState::Streaming {
            return ServerMessage::Error {
                code: ERR_NO_ACTIVE_SESSION,
                message: "No active ingest session.".into(),
            };
        }

        let doc_count = self.documents.len();
        tracing::info!(
            collection = self.collection.as_ref().map(|c| c.name.as_str()).unwrap_or("?"),
            docs_discarded = doc_count,
            "Ingest aborted"
        );

        self.state = SessionState::Idle;
        self.collection = None;
        self.documents.clear();
        self.batch_seq = 0;
        self.docs_received = 0;

        ServerMessage::Ack {
            batch_seq: 0,
            docs_received: 0,
        }
    }
}

/// A realtime write's answer, once its write-ahead log has it on disk.
async fn acknowledge(commit: Option<crate::persistence::wal::Commit>, doc_count: u32) -> ServerMessage {
    match commit {
        Some(commit) => match commit.wait().await {
            Ok(()) => ServerMessage::Ack { batch_seq: 0, docs_received: doc_count as u64 },
            Err(message) => ServerMessage::Error { code: ERR_INTERNAL, message },
        },
        None => ServerMessage::Ack { batch_seq: 0, docs_received: doc_count as u64 },
    }
}

fn write_error(e: WriteError) -> ServerMessage {
    match e {
        WriteError::Invalid(message) => ServerMessage::Error { code: ERR_SCHEMA_MISMATCH, message },
        WriteError::Log(message) => ServerMessage::Error { code: ERR_INTERNAL, message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{FieldDef, FieldType, FieldValue, Language, Schema};

    fn test_metrics() -> Arc<MetricsCollector> {
        Arc::new(MetricsCollector::new())
    }

    fn make_schema() -> Schema {
        let mut schema = Schema {
            name: "test".into(),
            fields: vec![FieldDef {
                name: "title".into(),
                field_type: FieldType::String,
                facet: false,
                index: true,
                optional: false,
                sort: false,
                stem: true,
                infix: false,
                locale: None,
                num_dim: None,
                vec_dist: None,
                id: 0,
                reference: None, drop: false,
                stem_dictionary: None,
                embed: None,
                name_matching: false,
            }],
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: Language::default(),
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        schema.assign_field_ids();
        schema
    }

    fn make_registry_with_collection(schema: Schema) -> CollectionRegistry {
        let reg = CollectionRegistry::new(None);
        reg.create(schema).unwrap();
        reg
    }

    fn make_start_ingest_bytes(collection: &str) -> Vec<u8> {
        serialize_client_message(&ClientMessage::StartIngest {
            collection: collection.into(),
            mode: 0,
        })
    }

    fn make_doc_batch_bytes(docs: &[Document], schema: &Schema) -> Vec<u8> {
        let batch = encode_doc_batch(docs, schema);
        serialize_client_message(&batch)
    }

    fn commit_bytes() -> Vec<u8> {
        serialize_client_message(&ClientMessage::Commit)
    }

    fn abort_bytes() -> Vec<u8> {
        serialize_client_message(&ClientMessage::Abort)
    }

    #[tokio::test]
    async fn idle_to_streaming() {
        let schema = make_schema();
        let reg = make_registry_with_collection(schema);
        let mut session = IngestSession::new(test_metrics());

        let resp = session.handle_message(&make_start_ingest_bytes("test"), &reg).await;
        assert!(matches!(resp, ServerMessage::Ack { batch_seq: 0, docs_received: 0 }));
        assert_eq!(session.state, SessionState::Streaming);
    }

    #[tokio::test]
    async fn start_ingest_unknown_collection() {
        let schema = make_schema();
        let reg = make_registry_with_collection(schema);
        let mut session = IngestSession::new(test_metrics());

        let resp = session.handle_message(&make_start_ingest_bytes("nope"), &reg).await;
        match resp {
            ServerMessage::Error { code, .. } => assert_eq!(code, ERR_COLLECTION_NOT_FOUND),
            _ => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn double_start_ingest_fails() {
        let schema = make_schema();
        let reg = make_registry_with_collection(schema);
        let mut session = IngestSession::new(test_metrics());

        session.handle_message(&make_start_ingest_bytes("test"), &reg).await;
        let resp = session.handle_message(&make_start_ingest_bytes("test"), &reg).await;
        match resp {
            ServerMessage::Error { code, .. } => assert_eq!(code, ERR_SESSION_ALREADY_ACTIVE),
            _ => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn doc_batch_without_session_fails() {
        let schema = make_schema();
        let reg = make_registry_with_collection(schema.clone());
        let mut session = IngestSession::new(test_metrics());

        let doc = Document {
            id: 0,
            external_id: "1".into(),
            language: Language::English,
            fields: vec![FieldValue::Text("hello".into())].into(),
            record: None,
        };
        let resp = session.handle_message(&make_doc_batch_bytes(&[doc], &schema), &reg).await;
        match resp {
            ServerMessage::Error { code, .. } => assert_eq!(code, ERR_NO_ACTIVE_SESSION),
            _ => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn commit_without_session_fails() {
        let reg = make_registry_with_collection(make_schema());
        let mut session = IngestSession::new(test_metrics());

        let resp = session.handle_message(&commit_bytes(), &reg).await;
        match resp {
            ServerMessage::Error { code, .. } => assert_eq!(code, ERR_NO_ACTIVE_SESSION),
            _ => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn abort_without_session_fails() {
        let reg = make_registry_with_collection(make_schema());
        let mut session = IngestSession::new(test_metrics());

        let resp = session.handle_message(&abort_bytes(), &reg).await;
        match resp {
            ServerMessage::Error { code, .. } => assert_eq!(code, ERR_NO_ACTIVE_SESSION),
            _ => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn full_ingest_flow() {
        let schema = make_schema();
        let reg = make_registry_with_collection(schema.clone());
        let mut session = IngestSession::new(test_metrics());

        // StartIngest
        session.handle_message(&make_start_ingest_bytes("test"), &reg).await;

        // DocBatch
        let docs: Vec<Document> = (0..5)
            .map(|i| Document {
                id: i,
                external_id: format!("{i}"),
                language: Language::English,
                fields: vec![FieldValue::Text(format!("doc {i}"))].into(),
                record: None,
            })
            .collect();
        let resp = session.handle_message(&make_doc_batch_bytes(&docs, &schema), &reg).await;
        match &resp {
            ServerMessage::Ack { batch_seq, docs_received } => {
                assert_eq!(*batch_seq, 1);
                assert_eq!(*docs_received, 5);
            }
            _ => panic!("expected Ack, got {resp:?}"),
        }

        // Commit — now returns Ack immediately (async rebuild)
        let resp = session.handle_message(&commit_bytes(), &reg).await;
        match resp {
            ServerMessage::Ack { batch_seq, docs_received } => {
                assert_eq!(batch_seq, 0);
                assert_eq!(docs_received, 5);
            }
            _ => panic!("expected Ack"),
        }

        // Wait for the background rebuild to complete
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Verify index was swapped
        let col = reg.get("test").unwrap();
        assert_eq!(col.snapshot().total_docs(), 5);
    }

    #[tokio::test]
    async fn abort_discards_docs() {
        let schema = make_schema();
        let reg = make_registry_with_collection(schema.clone());
        let mut session = IngestSession::new(test_metrics());

        session.handle_message(&make_start_ingest_bytes("test"), &reg).await;

        let docs = vec![Document {
            id: 0,
            external_id: "1".into(),
            language: Language::English,
            fields: vec![FieldValue::Text("hello".into())].into(),
            record: None,
        }];
        session.handle_message(&make_doc_batch_bytes(&docs, &schema), &reg).await;

        // Abort
        let resp = session.handle_message(&abort_bytes(), &reg).await;
        assert!(matches!(resp, ServerMessage::Ack { .. }));
        assert_eq!(session.state, SessionState::Idle);

        // Index should still be empty
        let col = reg.get("test").unwrap();
        assert_eq!(col.snapshot().total_docs(), 0);
    }

    #[tokio::test]
    async fn sequential_ingests_replace() {
        let schema = make_schema();
        let reg = make_registry_with_collection(schema.clone());
        let mut session = IngestSession::new(test_metrics());

        // First ingest: 3 docs
        session.handle_message(&make_start_ingest_bytes("test"), &reg).await;
        let docs: Vec<Document> = (0..3)
            .map(|i| Document {
                id: i,
                external_id: format!("{i}"),
                language: Language::English,
                fields: vec![FieldValue::Text(format!("first {i}"))].into(),
                record: None,
            })
            .collect();
        session.handle_message(&make_doc_batch_bytes(&docs, &schema), &reg).await;
        session.handle_message(&commit_bytes(), &reg).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(reg.get("test").unwrap().snapshot().total_docs(), 3);

        // Second ingest: 7 docs (replaces first)
        session.handle_message(&make_start_ingest_bytes("test"), &reg).await;
        let docs: Vec<Document> = (0..7)
            .map(|i| Document {
                id: i,
                external_id: format!("{i}"),
                language: Language::English,
                fields: vec![FieldValue::Text(format!("second {i}"))].into(),
                record: None,
            })
            .collect();
        session.handle_message(&make_doc_batch_bytes(&docs, &schema), &reg).await;
        session.handle_message(&commit_bytes(), &reg).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(reg.get("test").unwrap().snapshot().total_docs(), 7);
    }
}
