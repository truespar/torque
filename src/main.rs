#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(feature = "native", not(feature = "jemalloc"), target_env = "msvc"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use clap::Parser;
use std::sync::Arc;
use std::time::Instant;
use torque::auth::KeyStore;
use torque::collection::alias::AliasRegistry;
use torque::collection::CollectionRegistry;
use torque::config::{CliArgs, Config, ConfigSources};
use torque::memory::MemoryGuard;
use torque::observability::MetricsCollector;
use torque::persistence;
use torque::presets::PresetStore;
use torque::stopwords::StopwordStore;
use torque::synonyms::SynonymStore;
use tracing_subscriber::EnvFilter;

#[cfg(windows)]
mod win_service;

#[tokio::main]
async fn main() {
    let cli = CliArgs::parse();

    // Handle Windows Service commands early (before tracing init)
    #[cfg(windows)]
    {
        if cli.service_install {
            match win_service::install() {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("Failed to install service: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        if cli.service_uninstall {
            match win_service::uninstall() {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("Failed to uninstall service: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        if cli.service {
            match win_service::run_as_service() {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("Failed to start service: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
    }

    // Determine log level and log dir before consuming cli for config
    let log_level = cli
        .log_level
        .clone()
        .or_else(|| std::env::var("TORQUE_LOG_LEVEL").ok())
        .unwrap_or_else(|| "info".into());
    let log_dir = cli
        .log_dir
        .clone()
        .or_else(|| std::env::var("TORQUE_LOG_DIR").ok().map(std::path::PathBuf::from));

    // Initialize tracing — file + stdout when --log-dir is set, stdout-only otherwise
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&log_level));

    // Keep the guard alive for the lifetime of the process (dropped at end of main).
    // tracing-appender's non-blocking writer flushes on guard drop.
    let _log_guard: Option<tracing_appender::non_blocking::WorkerGuard>;

    if let Some(ref dir) = log_dir {
        // Create log directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("Failed to create log directory `{}`: {e}", dir.display());
            std::process::exit(1);
        }
        let file_appender = tracing_appender::rolling::daily(dir, "torque.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        _log_guard = Some(guard);

        // Write to both file (no ANSI) and stdout (with ANSI)
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(non_blocking);
        let stdout_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stdout);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .with(stdout_layer)
            .init();
    } else {
        _log_guard = None;
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .init();
    };

    // Install panic hook to log panics with structured context before aborting
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".into());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "non-string panic payload".into()
        };
        tracing::error!(
            thread = thread_name,
            location = location.as_str(),
            message = payload.as_str(),
            "PANIC"
        );
    }));

    let (config, sources) = match Config::load(cli) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    // Ensure data directory exists
    if let Err(e) = std::fs::create_dir_all(&config.data_dir) {
        eprintln!(
            "Failed to create data directory `{}`: {e}",
            config.data_dir.display()
        );
        std::process::exit(1);
    }

    // Acquire exclusive file lock on data directory
    let lock_file = match std::fs::File::create(config.data_dir.join("torque.lock")) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Failed to create lock file: {e}");
            std::process::exit(1);
        }
    };
    {
        use fs2::FileExt;
        if lock_file.try_lock_exclusive().is_err() {
            eprintln!(
                "Another Torque instance is using data directory: {}",
                config.data_dir.display()
            );
            std::process::exit(1);
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
        std::process::exit(1);
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
        Err(e) => {
            tracing::warn!("Failed to load aliases from disk: {e}");
        }
    }

    match persistence::load_synonyms(&config.data_dir) {
        Ok(syn_list) => {
            let count = syn_list.len();
            if count > 0 {
                synonyms.load(syn_list);
                tracing::info!(count, "Loaded synonym sets from disk");
            }
        }
        Err(e) => {
            tracing::warn!("Failed to load synonyms from disk: {e}");
        }
    }

    match persistence::load_curations(&config.data_dir) {
        Ok(cur_list) => {
            if !cur_list.is_empty() {
                tracing::info!(count = cur_list.len(), "Loaded curation sets from disk");
                curations.load(cur_list);
            }
        }
        Err(e) => {
            tracing::warn!("Failed to load curation sets from disk: {e}");
        }
    }

    match persistence::load_analytics_rules(&config.data_dir) {
        Ok(rules) => {
            if !rules.is_empty() {
                tracing::info!(count = rules.len(), "Loaded analytics rules from disk");
                analytics.load(rules);
            }
        }
        Err(e) => {
            tracing::warn!("Failed to load analytics rules from disk: {e}");
        }
    }

    match persistence::load_conversation_models(&config.data_dir) {
        Ok(models) => {
            if !models.is_empty() {
                tracing::info!(count = models.len(), "Loaded conversation models from disk");
                llm.load_conversation_models(models);
            }
        }
        Err(e) => {
            tracing::warn!("Failed to load conversation models from disk: {e}");
        }
    }

    match persistence::load_nl_search_models(&config.data_dir) {
        Ok(models) => {
            if !models.is_empty() {
                tracing::info!(count = models.len(), "Loaded NL search models from disk");
                llm.load_nl_models(models);
            }
        }
        Err(e) => {
            tracing::warn!("Failed to load NL search models from disk: {e}");
        }
    }

    match persistence::load_stemming_dictionaries(&config.data_dir) {
        Ok(dicts) => {
            let count = dicts.len();
            if count > 0 {
                torque::stemming::global().load(dicts);
                tracing::info!(count, "Loaded stemming dictionaries from disk");
            }
        }
        Err(e) => {
            tracing::warn!("Failed to load stemming dictionaries from disk: {e}");
        }
    }

    match persistence::load_presets(&config.data_dir) {
        Ok(preset_list) => {
            let count = preset_list.len();
            if count > 0 {
                presets.load(preset_list);
                tracing::info!(count, "Loaded presets from disk");
            }
        }
        Err(e) => {
            tracing::warn!("Failed to load presets from disk: {e}");
        }
    }

    match persistence::load_stopwords(&config.data_dir) {
        Ok(sw_list) => {
            let count = sw_list.len();
            if count > 0 {
                stopwords.load(sw_list);
                tracing::info!(count, "Loaded stopword sets from disk");
            }
        }
        Err(e) => {
            tracing::warn!("Failed to load stopwords from disk: {e}");
        }
    }

    match persistence::load_keys(&config.data_dir) {
        Ok(key_list) => {
            let count = key_list.len();
            if count > 0 {
                keys.load(key_list);
                tracing::info!(count, "Loaded API keys from disk");
            }
        }
        Err(e) => {
            tracing::warn!("Failed to load API keys from disk: {e}");
        }
    }

    // Parse ingest allowlist
    let ingest_allow: Vec<std::net::IpAddr> = config
        .ingest_allow
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();

    // Initialize metrics
    let metrics = Arc::new(MetricsCollector::new());
    let start_time = Instant::now();

    // Create memory guard (shares the metrics resident gauge)
    let memory_guard = MemoryGuard::new(
        config.max_memory_bytes,
        metrics.memory_resident.clone(),
    );

    // Spawn background stats collector
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

    // Graceful shutdown signal — handle both SIGINT (Ctrl+C) and SIGTERM (Docker/systemd)
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to register SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => tracing::info!("Received SIGINT (ctrl+c)"),
                _ = sigterm.recv() => tracing::info!("Received SIGTERM"),
            }
        }
        #[cfg(not(unix))]
        {
            ctrl_c.await.ok();
            tracing::info!("Received shutdown signal (ctrl+c)");
        }
        let _ = shutdown_tx.send(true);
    });

    if !config.quiet {
        print_startup_banner(
            &config,
            &sources,
            &collections,
            #[cfg(feature = "cuda")]
            &gpu,
        );
    }

    // Spawn compaction scheduler
    let _compaction_handle = torque::collection::compaction::spawn_compaction_scheduler(
        collections.clone(),
        Arc::clone(&metrics),
        Arc::new(config.clone()),
        shutdown_rx.clone(),
    );

    let ingest_addr = config.ingest_addr;
    let max_ingest_connections = config.max_ingest_connections;
    let shutdown_collections = collections.clone();
    let ingest_collections = collections.clone();
    let ingest_metrics = Arc::clone(&metrics);
    let ingest_memory_guard = memory_guard.clone();
    let ingest_data_dir = config.data_dir.clone();
    let ingest_shutdown_rx = shutdown_rx.clone();
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
        shutdown_rx,
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
                std::process::exit(1);
            }
        }
        result = ingest_fut => {
            if let Err(e) = result {
                tracing::error!("Ingest server error: {e}");
                std::process::exit(1);
            }
        }
    }

    // Graceful shutdown: every write the logs took is on disk once they close
    for col in shutdown_collections.list() {
        col.close_wal();
    }

    tracing::info!("Torque shut down cleanly");
}

fn print_startup_banner(
    config: &Config,
    sources: &ConfigSources,
    collections: &CollectionRegistry,
    #[cfg(feature = "cuda")] gpu: &Option<Arc<torque::gpu::GpuContext>>,
) {
    const DIM: &str = "\x1b[2m";
    const CYAN: &str = "\x1b[36m";
    const YELLOW: &str = "\x1b[33m";
    const RESET: &str = "\x1b[0m";

    fn display_host(addr: std::net::SocketAddr) -> String {
        if addr.ip().is_unspecified() {
            format!("localhost:{}", addr.port())
        } else {
            addr.to_string()
        }
    }

    fn source_tag(src: torque::config::ConfigSource) -> String {
        format!("{DIM}({src}){RESET}")
    }

    let version = env!("CARGO_PKG_VERSION");
    let api_host = display_host(config.api_addr);

    // ASCII art logo with version
    const COPYRIGHT: &str = "Copyright (c) 2025-2026 The Intelligence Company AB (publ). MIT OR Apache-2.0.";
    println!();
    println!("  {CYAN}▀█▀ ▄▀▄ █▀▄ ▄▀▄ █ █ █▀▀{RESET}   {DIM}v{version}{RESET}");
    println!("  {CYAN} █  █ █ █▄▀ █ █ █ █ █▀▀{RESET}   {DIM}{COPYRIGHT}{RESET}");
    println!("  {CYAN} ▀  ▀▀▀ ▀ ▀  ▀█ ▀▀▀ ▀▀▀{RESET}");
    println!();

    // Config sources section
    if let Some(ref file) = sources.config_file {
        println!("  {DIM}Config{RESET}       {CYAN}loaded {file}{RESET}");
    } else {
        println!("  {DIM}Config{RESET}       {DIM}see torque.example.toml{RESET}");
    }
    println!(
        "  {DIM}HTTP{RESET}         {CYAN}{}{RESET} {}",
        config.api_addr,
        source_tag(sources.api_addr)
    );
    println!(
        "  {DIM}TCP ingest{RESET}   {CYAN}{}{RESET} {}",
        config.ingest_addr,
        source_tag(sources.ingest_addr)
    );
    println!(
        "  {DIM}Data{RESET}         {CYAN}{}{RESET} {}",
        config.data_dir.display(),
        source_tag(sources.data_dir)
    );
    println!(
        "  {DIM}Memory{RESET}       {CYAN}{} MB{RESET} {}",
        config.max_memory_bytes / (1024 * 1024),
        source_tag(sources.memory)
    );
    if let Some(ref dir) = config.log_dir {
        println!(
            "  {DIM}Log dir{RESET}      {CYAN}{}{RESET}",
            dir.display()
        );
    }
    println!(
        "  {DIM}API key{RESET}      {YELLOW}{}{RESET} {}",
        &config.api_key,
        source_tag(sources.api_key)
    );
    println!();

    // Connection URLs
    let has_studio_files = config.studio_dir.is_some()
        || torque::server::has_embedded_studio();
    if has_studio_files {
        if config.studio_enabled {
            println!("  {DIM}Studio{RESET}       {CYAN}http://{api_host}{RESET}");
        } else {
            println!("  {DIM}Studio{RESET}       {DIM}(disabled){RESET}");
        }
    }
    #[cfg(feature = "mcp")]
    println!("  {DIM}MCP{RESET}          {CYAN}http://{api_host}/api/mcp{RESET}");
    println!();

    // Collections
    let col_list = collections.list();
    if col_list.is_empty() {
        println!("  {DIM}Collections{RESET}  {DIM}(none){RESET}");
    } else {
        for (i, col) in col_list.iter().enumerate() {
            let label = if i == 0 {
                format!("  {DIM}Collections{RESET}  ")
            } else {
                "               ".to_string()
            };
            let docs = col.snapshot_bundle().total_docs();
            println!("{label}{CYAN}{}{RESET} {DIM}({} docs){RESET}", col.name, format_count(docs as usize));
        }
    }
    println!();

    // GPU
    #[cfg(feature = "cuda")]
    {
        if let Some(g) = &gpu {
            println!(
                "  {DIM}GPU{RESET}          {CYAN}{} ({} MB VRAM){RESET}",
                g.device_name,
                g.vram_bytes / (1024 * 1024)
            );
        } else {
            println!("  {DIM}GPU{RESET}          {DIM}CPU fallback{RESET}");
        }
    }

    println!();
    println!("  {DIM}Docs{RESET}         {CYAN}https://truespar.com/torque/docs{RESET}");
    println!();
    println!("  {DIM}Press Ctrl+C to stop{RESET}");
    println!("  {DIM}Run with --help for all options{RESET}");
    println!();
}

fn format_count(n: usize) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        if m >= 100.0 {
            format!("{m:.0}M")
        } else {
            format!("{m:.1}M")
        }
    } else if n >= 1_000 {
        let k = n as f64 / 1_000.0;
        if k >= 100.0 {
            format!("{k:.0}K")
        } else {
            format!("{k:.1}K")
        }
    } else {
        n.to_string()
    }
}

