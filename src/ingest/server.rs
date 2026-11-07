use crate::collection::CollectionRegistry;
use crate::ingest::protocol::serialize_server_message;
use crate::ingest::session::IngestSession;
use crate::memory::MemoryGuard;
use crate::observability::MetricsCollector;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};
use tracing::Instrument;

static CONNECTION_COUNTER: AtomicU64 = AtomicU64::new(1);

pub async fn run(
    addr: SocketAddr,
    registry: Arc<CollectionRegistry>,
    allow_list: Vec<IpAddr>,
    metrics: Arc<MetricsCollector>,
    max_connections: usize,
    memory_guard: MemoryGuard,
    data_dir: PathBuf,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    #[cfg(feature = "cuda")] gpu: Option<Arc<crate::gpu::GpuContext>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("TCP ingest listening on {addr}");

    let semaphore = Arc::new(Semaphore::new(max_connections));

    loop {
        let (stream, peer_addr) = tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::error!(error = %e, "TCP accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                tracing::info!("TCP ingest server shutting down");
                break;
            }
        };

        // Disable Nagle's algorithm for lower latency on ACK frames.
        let _ = stream.set_nodelay(true);

        if !allow_list.is_empty() && !allow_list.contains(&peer_addr.ip()) {
            tracing::warn!(peer = %peer_addr, "Rejected connection: IP not in allowlist");
            drop(stream);
            continue;
        }

        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                tracing::warn!(
                    peer = %peer_addr,
                    max_connections,
                    "Rejected connection: max concurrent connections reached"
                );
                drop(stream);
                continue;
            }
        };

        let conn_id = CONNECTION_COUNTER.fetch_add(1, Ordering::Relaxed);
        metrics.tcp_connections_total.inc();
        tracing::debug!(peer = %peer_addr, conn_id, "Accepted ingest connection");

        let reg = Arc::clone(&registry);
        let conn_metrics = Arc::clone(&metrics);
        let conn_memory_guard = memory_guard.clone();
        let conn_data_dir = data_dir.clone();
        #[cfg(feature = "cuda")]
        let conn_gpu = gpu.clone();
        let span = tracing::info_span!("ingest", conn_id, peer = %peer_addr);
        tokio::spawn(async move {
            let _permit = permit; // held until task completes
            conn_metrics.ingest_active_sessions.inc();
            let result = handle_connection(
                stream, peer_addr, &reg, &conn_metrics, conn_memory_guard,
                conn_data_dir,
                #[cfg(feature = "cuda")]
                conn_gpu,
            ).await;
            conn_metrics.ingest_active_sessions.dec();
            if let Err(e) = result {
                tracing::debug!(peer = %peer_addr, conn_id, error = %e, "Connection closed");
            }
        }.instrument(span));
    }
    Ok(())
}

async fn handle_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    registry: &CollectionRegistry,
    metrics: &Arc<MetricsCollector>,
    memory_guard: MemoryGuard,
    data_dir: PathBuf,
    #[cfg(feature = "cuda")] gpu: Option<Arc<crate::gpu::GpuContext>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(feature = "cuda")]
    let mut session = IngestSession::with_gpu(Arc::clone(metrics), gpu)
        .with_memory_guard(memory_guard)
        .with_data_dir(data_dir);
    #[cfg(not(feature = "cuda"))]
    let mut session = IngestSession::new(Arc::clone(metrics))
        .with_memory_guard(memory_guard)
        .with_data_dir(data_dir);

    loop {
        // Only apply idle timeout when no active ingest session.
        // During active sessions, clients may spend hours preparing and sending data.
        let read_timeout = if session.is_active() {
            ACTIVE_SESSION_TIMEOUT
        } else {
            IDLE_TIMEOUT
        };

        let frame = match timeout(read_timeout, read_frame(&mut stream)).await {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    tracing::debug!(peer = %peer, "Client disconnected");
                    return Ok(());
                }
                return Err(e.into());
            }
            Err(_) => {
                if session.is_active() {
                    tracing::warn!(peer = %peer, "Active session timeout (1h no data), dropping connection");
                } else {
                    tracing::debug!(peer = %peer, "Idle timeout, dropping connection");
                }
                return Ok(());
            }
        };

        let response = session.handle_message(&frame, registry).await;
        let response_bytes = serialize_server_message(&response);
        match timeout(WRITE_TIMEOUT, write_frame(&mut stream, &response_bytes)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                tracing::warn!(peer = %peer, "Write timeout (30s), dropping slow client");
                return Ok(());
            }
        }
    }
}

/// Idle timeout: drop connections with no active session after 5 minutes of inactivity.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Active session timeout: drop connections that haven't sent data for 1 hour.
/// Prevents dead/stuck clients from holding semaphore permits indefinitely.
const ACTIVE_SESSION_TIMEOUT: Duration = Duration::from_secs(3600);

/// Write timeout: prevent slow/stuck clients from blocking the tokio task.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum allowed frame size (1 GB). A single frame carries a batch of documents
/// (typically 5000), which can be 50-100 MB for wide schemas. 1 GB accommodates
/// very large batches while preventing unbounded allocation from malformed headers.
const MAX_FRAME_SIZE: usize = 1024 * 1024 * 1024;

async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>, std::io::Error> {
    let len = stream.read_u32().await? as usize;
    if len == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "zero-length frame",
        ));
    }
    if len > MAX_FRAME_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame too large: {len} bytes exceeds {MAX_FRAME_SIZE} byte limit"),
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_frame(stream: &mut TcpStream, data: &[u8]) -> Result<(), std::io::Error> {
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}
