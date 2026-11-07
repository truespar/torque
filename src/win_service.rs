//! Windows Service support for Torque.
//!
//! Provides install, uninstall, and service-mode execution so that
//! Torque can run as a native Windows Service.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;

use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const SERVICE_NAME: &str = "TorqueServer";
const SERVICE_DISPLAY_NAME: &str = "Torque Search Engine";
const SERVICE_DESCRIPTION: &str =
    "Torque in-memory search engine \u{2014} HTTP API, TCP ingest, and Studio";

/// Install the service in the Windows Service Control Manager.
pub fn install() -> windows_service::Result<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;

    let exe_path = std::env::current_exe().expect("failed to get executable path");

    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe_path,
        launch_arguments: vec![OsString::from("--service")],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };

    let service = manager.create_service(&service_info, ServiceAccess::CHANGE_CONFIG)?;
    service.set_description(SERVICE_DESCRIPTION)?;

    println!("Service '{SERVICE_NAME}' installed successfully.");
    println!();
    println!("  Configure via:  torque.toml in the executable directory,");
    println!("                  or environment variables (TORQUE_API_KEY, etc.)");
    println!();
    println!("  Start:          sc start {SERVICE_NAME}");
    println!("                  net start {SERVICE_NAME}");
    println!();
    println!("  Stop:           sc stop {SERVICE_NAME}");
    println!("  Remove:         torque --service-uninstall");

    Ok(())
}

/// Uninstall the service from the Windows Service Control Manager.
pub fn uninstall() -> windows_service::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;

    let service_access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = manager.open_service(SERVICE_NAME, service_access)?;

    // Stop the service if running
    if let Ok(status) = service.query_status() {
        if status.current_state != ServiceState::Stopped {
            println!("Stopping service...");
            service.stop()?;
        }
    }

    service.delete()?;

    // Wait for deletion to complete
    let start = Instant::now();
    let timeout = Duration::from_secs(10);
    while start.elapsed() < timeout {
        if let Err(windows_service::Error::Winapi(e)) =
            manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)
        {
            let raw = e.raw_os_error().unwrap_or(0);
            // ERROR_SERVICE_DOES_NOT_EXIST = 1060
            if raw == 1060 {
                println!("Service '{SERVICE_NAME}' removed successfully.");
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    println!("Service '{SERVICE_NAME}' marked for deletion (will complete after reboot or when all handles are closed).");
    Ok(())
}

// -- Service dispatch ---------------------------------------------------------

windows_service::define_windows_service!(ffi_service_main, service_main);

/// Entry point called by the Windows Service Control Manager.
pub fn run_as_service() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

/// The actual service main function, called by the SCM dispatcher.
fn service_main(_arguments: Vec<OsString>) {
    if let Err(e) = run_service() {
        tracing::error!("service exited with error: {e}");
    }
}

fn run_service() -> Result<(), Box<dyn std::error::Error>> {
    // Set up tracing to write to a rolling log file (no console in service mode)
    let log_dir = std::env::var("TORQUE_LOG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(|| std::path::PathBuf::from("."))
        });
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = tracing_appender::rolling::daily(&log_dir, "torque.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();

    tracing::info!("Torque server starting as Windows service");

    // Create a shutdown channel
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();

    // Register the service control handler
    let status_handle = service_control_handler::register(SERVICE_NAME, move |control| {
        match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                shutdown_tx.send(()).ok();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    })?;

    // Report: starting
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::StartPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(30),
        process_id: None,
    })?;

    // Build and start the server in a tokio runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let status_handle_clone = status_handle.clone();
    let result = rt.block_on(async move {
        match run_server_inner(status_handle_clone, shutdown_rx).await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::error!("server error: {e}");
                Err(e)
            }
        }
    });

    // Report: stopped
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(if result.is_ok() { 0 } else { 1 }),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    Ok(())
}

/// Runs the server with the same logic as the interactive `main()`, but uses
/// the SCM shutdown receiver instead of Ctrl+C.
async fn run_server_inner(
    status_handle: windows_service::service_control_handler::ServiceStatusHandle,
    shutdown_rx: std::sync::mpsc::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    use torque::auth::KeyStore;
    use torque::collection::alias::AliasRegistry;
    use torque::collection::CollectionRegistry;
    use torque::config::{CliArgs, Config};
    use torque::memory::MemoryGuard;
    use torque::observability::MetricsCollector;
    use torque::persistence;
    use torque::presets::PresetStore;
    use torque::stopwords::StopwordStore;
    use torque::synonyms::SynonymStore;

    let cli = CliArgs::parse();
    let (config, _sources) = Config::load(cli).map_err(|e| format!("config error: {e}"))?;

    // Ensure data directory exists
    std::fs::create_dir_all(&config.data_dir)
        .map_err(|e| format!("failed to create data dir: {e}"))?;

    // Acquire exclusive file lock on data directory
    let lock_file = std::fs::File::create(config.data_dir.join("torque.lock"))
        .map_err(|e| format!("failed to create lock file: {e}"))?;
    {
        use fs2::FileExt;
        if lock_file.try_lock_exclusive().is_err() {
            return Err(format!(
                "Another Torque instance is using data directory: {}",
                config.data_dir.display()
            ).into());
        }
    }
    // lock_file must survive for entire process lifetime — hold the binding

    // Load persisted state
    let collections = Arc::new(CollectionRegistry::new(Some(config.data_dir.clone())));
    let aliases = Arc::new(AliasRegistry::new());
    let synonyms = Arc::new(SynonymStore::new());
    let curations = Arc::new(torque::curation::CurationStore::new());
    let analytics = Arc::new(torque::analytics::AnalyticsStore::new());
    let llm = Arc::new(torque::llm::LlmRegistry::new());
    let presets = Arc::new(PresetStore::new());
    let stopwords = Arc::new(StopwordStore::new());
    let keys = Arc::new(KeyStore::new(config.api_key.clone()));

    if let Err(e) = persistence::load_collections(&config.data_dir, &collections, None) {
        // started without them, the first save of collections.json would lose them
        tracing::error!("The collections could not be loaded, Torque does not start: {e}");
        return Err(e.into());
    } else {
        let count = collections.list().len();
        if count > 0 {
            tracing::info!(count, "Loaded collections from disk");
        }
    }

    match persistence::load_aliases(&config.data_dir) {
        Ok(alias_list) => {
            let count = alias_list.len();
            if count > 0 {
                aliases.load(alias_list);
                tracing::info!(count, "Loaded aliases from disk");
            }
        }
        Err(e) => tracing::warn!("Failed to load aliases from disk: {e}"),
    }

    match persistence::load_synonyms(&config.data_dir) {
        Ok(syn_list) => {
            let count = syn_list.len();
            if count > 0 {
                synonyms.load(syn_list);
                tracing::info!(count, "Loaded synonym sets from disk");
            }
        }
        Err(e) => tracing::warn!("Failed to load synonyms from disk: {e}"),
    }

    match persistence::load_curations(&config.data_dir) {
        Ok(cur_list) => {
            let count = cur_list.len();
            if count > 0 {
                curations.load(cur_list);
                tracing::info!(count, "Loaded curation sets from disk");
            }
        }
        Err(e) => tracing::warn!("Failed to load curation sets from disk: {e}"),
    }

    match persistence::load_analytics_rules(&config.data_dir) {
        Ok(rules) => {
            let count = rules.len();
            if count > 0 {
                analytics.load(rules);
                tracing::info!(count, "Loaded analytics rules from disk");
            }
        }
        Err(e) => tracing::warn!("Failed to load analytics rules from disk: {e}"),
    }

    match persistence::load_conversation_models(&config.data_dir) {
        Ok(models) => {
            let count = models.len();
            if count > 0 {
                llm.load_conversation_models(models);
                tracing::info!(count, "Loaded conversation models from disk");
            }
        }
        Err(e) => tracing::warn!("Failed to load conversation models from disk: {e}"),
    }

    match persistence::load_nl_search_models(&config.data_dir) {
        Ok(models) => {
            let count = models.len();
            if count > 0 {
                llm.load_nl_models(models);
                tracing::info!(count, "Loaded NL search models from disk");
            }
        }
        Err(e) => tracing::warn!("Failed to load NL search models from disk: {e}"),
    }

    match persistence::load_stemming_dictionaries(&config.data_dir) {
        Ok(dicts) => {
            let count = dicts.len();
            if count > 0 {
                torque::stemming::global().load(dicts);
                tracing::info!(count, "Loaded stemming dictionaries from disk");
            }
        }
        Err(e) => tracing::warn!("Failed to load stemming dictionaries from disk: {e}"),
    }

    match persistence::load_presets(&config.data_dir) {
        Ok(preset_list) => {
            let count = preset_list.len();
            if count > 0 {
                presets.load(preset_list);
                tracing::info!(count, "Loaded presets from disk");
            }
        }
        Err(e) => tracing::warn!("Failed to load presets from disk: {e}"),
    }

    match persistence::load_stopwords(&config.data_dir) {
        Ok(sw_list) => {
            let count = sw_list.len();
            if count > 0 {
                stopwords.load(sw_list);
                tracing::info!(count, "Loaded stopword sets from disk");
            }
        }
        Err(e) => tracing::warn!("Failed to load stopwords from disk: {e}"),
    }

    match persistence::load_keys(&config.data_dir) {
        Ok(key_list) => {
            let count = key_list.len();
            if count > 0 {
                keys.load(key_list);
                tracing::info!(count, "Loaded API keys from disk");
            }
        }
        Err(e) => tracing::warn!("Failed to load API keys from disk: {e}"),
    }

    // Parse ingest allowlist
    let ingest_allow: Vec<std::net::IpAddr> = config
        .ingest_allow
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();

    // Initialize metrics
    let metrics = Arc::new(MetricsCollector::new());
    let start_time = std::time::Instant::now();

    let memory_guard = MemoryGuard::new(
        config.max_memory_bytes,
        metrics.memory_resident.clone(),
    );

    torque::observability::stats::spawn_stats_collector(
        Arc::clone(&metrics),
        Arc::clone(&collections),
    );

    // Initialize GPU context (CUDA feature only)
    #[cfg(feature = "cuda")]
    let gpu = {
        let g = torque::gpu::GpuContext::try_init(config.gpu_vram_budget_bytes).map(Arc::new);
        if let Some(ref gpu) = g {
            metrics.gpu_vram_total_bytes.set(gpu.vram_bytes as i64);
        } else {
            tracing::info!("No GPU available, using CPU fallback");
        }
        g
    };

    // Bridge SCM shutdown signal to tokio watch channel
    let (watch_tx, watch_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        tokio::task::spawn_blocking(move || {
            shutdown_rx.recv().ok();
        })
        .await
        .ok();
        tracing::info!("Service stop requested, shutting down gracefully");
        let _ = watch_tx.send(true);
    });

    // Report: running
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    tracing::info!("Torque server running as Windows service");

    let ingest_addr = config.ingest_addr;
    let max_ingest_connections = config.max_ingest_connections;
    let ingest_collections = collections.clone();
    let ingest_metrics = Arc::clone(&metrics);
    let ingest_memory_guard = memory_guard.clone();
    let ingest_data_dir = config.data_dir.clone();
    let ingest_shutdown_rx = watch_rx.clone();
    #[cfg(feature = "cuda")]
    let ingest_gpu = gpu.clone();

    // TLS setup
    #[cfg(feature = "tls")]
    let tls_acceptor = {
        if let (Some(cert_path), Some(key_path)) = (&config.tls_cert, &config.tls_key) {
            use std::io::BufReader;
            use tokio_rustls::rustls;

            rustls::crypto::ring::default_provider()
                .install_default()
                .ok();

            let cert_file = std::fs::File::open(cert_path)
                .map_err(|e| format!("Failed to open TLS cert {cert_path}: {e}")).unwrap();
            let key_file = std::fs::File::open(key_path)
                .map_err(|e| format!("Failed to open TLS key {key_path}: {e}")).unwrap();

            let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_file))
                .collect::<Result<Vec<_>, _>>().unwrap();
            let key = rustls_pemfile::private_key(&mut BufReader::new(key_file))
                .unwrap()
                .expect("No private key found in TLS key file");

            let tls_config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap();

            tracing::info!(cert = %cert_path, key = %key_path, "TLS enabled");
            Some(tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(tls_config)))
        } else {
            None
        }
    };

    let http_fut = torque::server::run(
        config,
        collections,
        aliases,
        synonyms,
        curations,
        analytics,
        llm,
        presets,
        stopwords,
        keys,
        metrics,
        start_time,
        memory_guard,
        watch_rx,
        #[cfg(feature = "cuda")]
        gpu,
        #[cfg(feature = "tls")]
        tls_acceptor,
    );
    let ingest_fut = torque::ingest::server::run(
        ingest_addr,
        ingest_collections,
        ingest_allow,
        ingest_metrics,
        max_ingest_connections,
        ingest_memory_guard,
        ingest_data_dir,
        ingest_shutdown_rx,
        #[cfg(feature = "cuda")]
        ingest_gpu,
    );

    tokio::select! {
        result = http_fut => {
            if let Err(e) = result {
                tracing::error!("HTTP server error: {e}");
            }
        }
        result = ingest_fut => {
            if let Err(e) = result {
                tracing::error!("Ingest server error: {e}");
            }
        }
    }

    tracing::info!("Torque shut down cleanly");
    Ok(())
}
