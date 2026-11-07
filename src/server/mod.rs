pub mod ai;
pub mod error;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod response;
pub mod routes;
pub mod segments;
#[cfg(test)]
mod metrics_tests;
#[cfg(test)]
mod model_tests;

use crate::auth::middleware::auth_middleware;
use crate::auth::KeyStore;
use crate::collection::alias::AliasRegistry;
use crate::collection::CollectionRegistry;
use crate::config::Config;
use crate::memory::MemoryGuard;
use crate::observability::MetricsCollector;
use crate::presets::PresetStore;
use crate::stopwords::StopwordStore;
use crate::synonyms::SynonymStore;
use axum::extract::{MatchedPath, Request, State};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::Router;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::ServeDir;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::Instrument;

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

/// TTL-bounded search result cache. Entries are keyed on a hash of the
/// collection, raw query string, and the bundle's Arc address, so any write
/// (which publishes a new bundle) misses naturally. Each entry holds the bundle
/// alive, which makes address-based keying sound (an address can't be reused
/// for a new bundle while the entry pins the old one) at the cost of keeping
/// superseded index state alive for at most the TTL.
struct CacheEntry {
    expires: std::time::Instant,
    value: Arc<serde_json::Value>,
    _bundle_guard: Arc<crate::index::IndexBundle>,
}

#[derive(Default)]
pub struct SearchCache {
    map: std::sync::Mutex<rustc_hash::FxHashMap<u64, CacheEntry>>,
}

impl SearchCache {
    const MAX_ENTRIES: usize = 1024;

    pub fn get(&self, key: u64) -> Option<Arc<serde_json::Value>> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(&key) {
            Some(entry) if entry.expires < std::time::Instant::now() => {
                map.remove(&key);
                None
            }
            Some(entry) => Some(Arc::clone(&entry.value)),
            None => None,
        }
    }

    pub fn insert(
        &self,
        key: u64,
        ttl: std::time::Duration,
        value: Arc<serde_json::Value>,
        bundle: Arc<crate::index::IndexBundle>,
    ) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= Self::MAX_ENTRIES {
            let now = std::time::Instant::now();
            map.retain(|_, entry| entry.expires >= now);
            if map.len() >= Self::MAX_ENTRIES {
                map.clear();
            }
        }
        map.insert(
            key,
            CacheEntry {
                expires: std::time::Instant::now() + ttl,
                value,
                _bundle_guard: bundle,
            },
        );
    }

    pub fn clear(&self) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

pub struct AppState {
    pub collections: Arc<CollectionRegistry>,
    pub aliases: Arc<AliasRegistry>,
    pub synonyms: Arc<SynonymStore>,
    pub curations: Arc<crate::curation::CurationStore>,
    pub analytics: Arc<crate::analytics::AnalyticsStore>,
    pub llm: Arc<crate::llm::LlmRegistry>,
    pub presets: Arc<PresetStore>,
    pub stopwords: Arc<StopwordStore>,
    pub keys: Arc<KeyStore>,
    pub config: Arc<Config>,
    pub metrics: Arc<MetricsCollector>,
    pub start_time: Instant,
    pub memory_guard: MemoryGuard,
    pub ip_filter: Arc<crate::ip_filter::IpFilter>,
    /// Serializes the handlers that make or remove a collection's files. DELETE's cleanup keeps it
    /// until the files are gone (`lock_owned`, the guard moved into the cleanup), so a CREATE or
    /// import of the same name waits instead of losing its new files to the cleanup.
    pub fs_serial: Arc<tokio::sync::Mutex<()>>,
    /// Runtime-adjustable slow-query threshold (ms, 0 = disabled).
    /// Initialized from config; changed via POST /config
    /// (`log-slow-requests-time-ms`, Typesense v30.1).
    pub slow_query_ms: std::sync::atomic::AtomicU64,
    /// TTL-bounded search result cache (use_cache/cache_ttl, Typesense v30.1).
    pub search_cache: SearchCache,
    #[cfg(feature = "cuda")]
    pub gpu: Option<Arc<crate::gpu::GpuContext>>,
}

/// Embedded studio files (from studio/dist/ at build time, if present).
static EMBEDDED_STUDIO: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/studio/dist");

/// Check if studio files are embedded at compile time.
pub fn has_embedded_studio() -> bool {
    EMBEDDED_STUDIO.contains("index.html")
}

pub async fn run(
    config: Config,
    collections: Arc<CollectionRegistry>,
    aliases: Arc<AliasRegistry>,
    synonyms: Arc<SynonymStore>,
    curations: Arc<crate::curation::CurationStore>,
    analytics: Arc<crate::analytics::AnalyticsStore>,
    llm: Arc<crate::llm::LlmRegistry>,
    presets: Arc<PresetStore>,
    stopwords: Arc<StopwordStore>,
    keys: Arc<KeyStore>,
    metrics: Arc<MetricsCollector>,
    start_time: Instant,
    memory_guard: MemoryGuard,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    #[cfg(feature = "cuda")] gpu: Option<Arc<crate::gpu::GpuContext>>,
    #[cfg(feature = "tls")] tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
) -> Result<(), Box<dyn std::error::Error>> {
    let cors = build_cors(&config.cors_origins);
    let addr = config.api_addr;
    let max_http_body_bytes = config.max_http_body_bytes;
    let studio_dir = config.studio_dir.clone();
    let studio_enabled = config.studio_enabled;
    let slow_query_ms_init = config.slow_query_ms;

    // Parse the IP allowlist; invalid entries log and fall back to allow-all
    // rather than taking the server offline.
    let ip_filter = Arc::new(
        crate::ip_filter::IpFilter::parse(&config.allowed_ips).unwrap_or_else(|e| {
            tracing::error!(
                "invalid allowed_ips setting `{}`: {e}. Falling back to allow-all.",
                config.allowed_ips
            );
            crate::ip_filter::IpFilter::allow_all()
        }),
    );
    if !ip_filter.is_empty() {
        tracing::info!(
            ranges = ip_filter.len(),
            "HTTP IP allowlist active",
        );
    }

    let state = Arc::new(AppState {
        collections,
        aliases,
        synonyms,
        curations,
        analytics,
        llm,
        presets,
        stopwords,
        keys,
        config: Arc::new(config),
        metrics,
        start_time,
        memory_guard,
        ip_filter,
        fs_serial: Arc::new(tokio::sync::Mutex::new(())),
        slow_query_ms: std::sync::atomic::AtomicU64::new(slow_query_ms_init),
        search_cache: SearchCache::default(),
        #[cfg(feature = "cuda")]
        gpu,
    });

    if let Some(ai) = &state.config.ai {
        tracing::info!(provider = %ai.provider, model = ai.model.as_str(), "Search Assistant is on");
    }

    // One auth layer over everything: the API, and Studio's own routes when Studio is enabled.
    let mut api_routes = routes::routes();
    if studio_enabled {
        api_routes = api_routes.merge(routes::studio_routes());
    }
    let api_routes = api_routes.layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    let request_timeout = std::time::Duration::from_millis(state.config.query_timeout_ms * 3);

    let compression = CompressionLayer::new().gzip(true).zstd(true);

    let app = Router::new().merge(api_routes);

    let mut app = app
        .layer(middleware::from_fn_with_state(
            state.clone(),
            request_id_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            ip_filter_middleware,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(max_http_body_bytes as usize))
        .layer(RequestBodyLimitLayer::new(max_http_body_bytes as usize))
        .layer(TimeoutLayer::with_status_code(axum::http::StatusCode::REQUEST_TIMEOUT, request_timeout))
        .layer(compression)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        // Segment uploads stream large bodies for minutes: outside the request timeout.
        .merge(
            segments::routes()
                .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
                .layer(middleware::from_fn_with_state(state.clone(), request_id_middleware))
                .layer(middleware::from_fn_with_state(state.clone(), ip_filter_middleware))
                .layer(TraceLayer::new_for_http()),
        )
        .with_state(state.clone());

    // Static file serving for studio SPA (only when studio is enabled)
    if studio_enabled {
        if let Some(ref dir) = studio_dir {
            tracing::info!(dir = %dir.display(), "Serving studio from filesystem");
            let index_path = dir.join("index.html");
            let index_html = std::fs::read_to_string(&index_path).unwrap_or_default();
            let serve = ServeDir::new(dir)
                .fallback(axum::routing::get(move || async move { Html(index_html) }));
            app = app.fallback_service(serve);
        } else if EMBEDDED_STUDIO.contains("index.html") {
            tracing::info!("Serving studio from embedded files");
            app = app.fallback(axum::routing::get(serve_embedded_studio));
        }
    } else {
        tracing::info!("Studio is disabled");
    }

    // socket2 for SO_REUSEADDR, TCP_NODELAY and a large backlog.
    let listener = {
        let socket = socket2::Socket::new(
            if addr.is_ipv6() { socket2::Domain::IPV6 } else { socket2::Domain::IPV4 },
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )?;
        socket.set_reuse_address(true)?;
        socket.set_tcp_nodelay(true)?;
        socket.set_nonblocking(true)?;
        socket.bind(&addr.into())?;
        socket.listen(1024)?;
        tokio::net::TcpListener::from_std(socket.into())?
    };
    // TLS mode: manual accept loop with tokio-rustls
    #[cfg(feature = "tls")]
    if let Some(acceptor) = tls_acceptor {
        tracing::info!("HTTPS (TLS) listening on {addr}");
        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, remote_addr) = match result {
                        Ok(pair) => pair,
                        Err(e) => {
                            // An accept error never ends the server loop: log, back off briefly so a
                            // persistently bad fd does not spin, and go on.
                            tracing::warn!("accept error on {addr}: {e}");
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            continue;
                        }
                    };
                    let acceptor = acceptor.clone();
                    let app = app.clone();
                    tokio::spawn(async move {
                        let tls_stream = match acceptor.accept(stream).await {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::debug!("TLS handshake failed from {remote_addr}: {e}");
                                return;
                            }
                        };
                        let io = hyper_util::rt::TokioIo::new(tls_stream);
                        // Inject ConnectInfo so auth middleware can extract peer address
                        let app = app.layer(axum::Extension(
                            axum::extract::connect_info::ConnectInfo(remote_addr),
                        ));
                        let svc = hyper_util::service::TowerToHyperService::new(app);
                        if let Err(e) = hyper_util::server::conn::auto::Builder::new(
                            hyper_util::rt::TokioExecutor::new(),
                        )
                        .serve_connection(io, svc)
                        .await
                        {
                            tracing::debug!("HTTP connection error from {remote_addr}: {e}");
                        }
                    });
                }
                _ = shutdown_rx.changed() => {
                    tracing::info!("HTTPS server shutting down gracefully");
                    break;
                }
            }
        }
        return Ok(());
    }

    // Plain HTTP mode (default).
    tracing::info!("HTTP API listening on {addr}");
    let make_svc = app.into_make_service_with_connect_info::<std::net::SocketAddr>();

    {
        axum::serve(listener, make_svc)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.changed().await;
                tracing::info!("HTTP server shutting down gracefully");
            })
            .await?;
        Ok(())
    }
}

/// Serve studio files from embedded include_dir.
async fn serve_embedded_studio(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');

    // Try to serve the exact file
    if let Some(file) = EMBEDDED_STUDIO.get_file(path) {
        let mime = mime_from_path(path);
        return (
            [(axum::http::header::CONTENT_TYPE, mime)],
            file.contents(),
        ).into_response();
    }

    // SPA fallback: serve index.html for non-file routes
    if let Some(file) = EMBEDDED_STUDIO.get_file("index.html") {
        return (
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            file.contents(),
        ).into_response();
    }

    axum::http::StatusCode::NOT_FOUND.into_response()
}

fn mime_from_path(path: &str) -> &'static str {
    if path.ends_with(".js") { "application/javascript" }
    else if path.ends_with(".css") { "text/css" }
    else if path.ends_with(".html") { "text/html; charset=utf-8" }
    else if path.ends_with(".svg") { "image/svg+xml" }
    else if path.ends_with(".png") { "image/png" }
    else if path.ends_with(".json") { "application/json" }
    else if path.ends_with(".woff2") { "font/woff2" }
    else if path.ends_with(".woff") { "font/woff" }
    else if path.ends_with(".wasm") { "application/wasm" }
    else { "application/octet-stream" }
}

/// Reject requests whose peer IP is not in the `allowed_ips` allowlist.
/// When the allowlist is empty, passes through untouched.
async fn ip_filter_middleware(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if !state.ip_filter.is_allowed(&peer.ip()) {
        tracing::warn!(peer = %peer, path = %request.uri().path(), "HTTP access denied: IP not in allowlist");
        return (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({"message": "Access denied: IP not in allowlist"})),
        )
            .into_response();
    }
    next.run(request).await
}

async fn request_id_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    request.extensions_mut().insert(RequestId(request_id));

    // Record HTTP request metric, by the route matched: a path holds document ids and collection
    // names, and each value would stay a series in /metrics for good
    let method = request.method().to_string();
    let path = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_string(), |p| p.as_str().to_string());
    state
        .metrics
        .http_requests_total
        .get_or_create(&crate::observability::HttpRequestLabels { method, path })
        .inc();

    let span = tracing::info_span!("request", rid = request_id);

    let mut response = next.run(request).instrument(span).await;
    response.headers_mut().insert(
        "x-request-id",
        axum::http::HeaderValue::from_str(&request_id.to_string()).expect("numeric request id"),
    );
    response
}

/// Request correlation ID, accessible via request extensions.
#[derive(Debug, Clone, Copy)]
pub struct RequestId(pub u64);

fn build_cors(origins: &[String]) -> CorsLayer {
    if origins.len() == 1 && origins[0] == "*" {
        CorsLayer::permissive()
    } else {
        let origins: Vec<_> = origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new().allow_origin(AllowOrigin::list(origins))
    }
}
