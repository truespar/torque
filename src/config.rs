#[cfg(feature = "native")]
use clap::Parser;
use serde::Deserialize;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

const DEFAULT_API_ADDR: &str = "0.0.0.0:8108";
const DEFAULT_INGEST_ADDR: &str = "0.0.0.0:8109";

#[cfg(feature = "native")]
#[derive(Parser, Debug)]
#[command(name = "torque", about = "Truespar Torque — in-memory search engine")]
pub struct CliArgs {
    /// Bootstrap API key (required)
    #[arg(long)]
    pub api_key: Option<String>,

    /// HTTP API listen address
    #[arg(long)]
    pub api_addr: Option<SocketAddr>,

    /// TCP ingest listen address
    #[arg(long)]
    pub ingest_addr: Option<SocketAddr>,

    /// Data directory for persisted JSON (collections, aliases, keys)
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long)]
    pub log_level: Option<String>,

    /// Directory for log files (daily rotation). Logs go to stdout when unset.
    #[arg(long)]
    pub log_dir: Option<PathBuf>,

    /// CORS allowed origins (comma-separated)
    #[arg(long)]
    pub cors_origins: Option<String>,

    /// GPU VRAM budget for dense bitmaps in MB (0 = auto-detect 50% of device VRAM)
    #[arg(long)]
    pub gpu_vram_budget_mb: Option<u64>,

    /// Maximum vector memory in GB (prevents OOM on large vector indexes)
    #[arg(long)]
    pub max_vector_memory_gb: Option<u64>,

    /// Maximum HTTP request body size in bytes (default: unlimited)
    #[arg(long)]
    pub max_http_body_bytes: Option<u64>,

    /// Maximum memory usage as percentage of system RAM (default: 90)
    #[arg(long)]
    pub max_memory_percent: Option<u64>,

    /// Maximum memory usage in bytes (overrides max_memory_percent)
    #[arg(long)]
    pub max_memory_bytes: Option<u64>,

    /// Maximum concurrent TCP ingest connections (default: 64)
    #[arg(long)]
    pub max_ingest_connections: Option<usize>,

    /// Query timeout in milliseconds (default: 10000)
    #[arg(long)]
    pub query_timeout_ms: Option<u64>,

    /// Path to TOML config file
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Path to pre-built studio dist directory (overrides embedded files)
    #[arg(long)]
    pub studio_dir: Option<PathBuf>,

    /// TLS certificate file path (PEM format)
    #[cfg(feature = "tls")]
    #[arg(long)]
    pub tls_cert: Option<String>,

    /// TLS private key file path (PEM format)
    #[cfg(feature = "tls")]
    #[arg(long)]
    pub tls_key: Option<String>,

    /// Slow query logging threshold in milliseconds (0 = disabled)
    #[arg(long)]
    pub slow_query_ms: Option<u64>,

    /// Allow DROP COLLECTION via Studio and HTTP API (default: enabled)
    #[arg(long)]
    pub allow_drop_collection: bool,

    /// Disable the embedded Studio web UI (default: enabled)
    #[arg(long)]
    pub no_studio: bool,

    /// Suppress the startup banner. For unattended/cloud deployments where
    /// stdout is captured and shipped — the banner prints the API key and other
    /// config, which must never reach customer-visible logs.
    #[arg(long)]
    pub quiet: bool,

    /// Comma-separated CIDR allowlist for HTTP API clients (e.g. `10.0.0.0/8,203.0.113.42`).
    /// Empty allows all connections (default).
    #[arg(long)]
    pub allowed_ips: Option<String>,

    /// Compaction interval in seconds (default: 1800 = 30 min, 0 = disabled)
    #[arg(long)]
    pub compaction_interval_secs: Option<u64>,

    /// Memory pressure compaction threshold as percent of max_memory_bytes (default: 80, 0 = disabled)
    #[arg(long)]
    pub compaction_memory_threshold_percent: Option<u64>,

    /// Install Torque as a Windows Service
    #[cfg(windows)]
    #[arg(long)]
    pub service_install: bool,

    /// Uninstall Torque Windows Service
    #[cfg(windows)]
    #[arg(long)]
    pub service_uninstall: bool,

    /// Run as a Windows Service (used by SCM, hidden from --help)
    #[cfg(windows)]
    #[arg(long, hide = true)]
    pub service: bool,
}

#[derive(Debug, Deserialize, Default)]
pub struct TomlConfig {
    pub api_key: Option<String>,
    pub api_addr: Option<SocketAddr>,
    pub ingest_addr: Option<SocketAddr>,
    pub data_dir: Option<PathBuf>,
    pub log_level: Option<String>,
    pub log_dir: Option<PathBuf>,
    pub cors_origins: Option<Vec<String>>,
    pub ingest_allow: Option<Vec<String>>,
    pub gpu_vram_budget_mb: Option<u64>,
    pub max_vector_memory_gb: Option<u64>,
    pub max_http_body_bytes: Option<u64>,
    pub max_memory_percent: Option<u64>,
    pub max_memory_bytes: Option<u64>,
    pub max_ingest_connections: Option<usize>,
    pub query_timeout_ms: Option<u64>,
    /// Studio's Search Assistant: `anthropic` (the Claude API) or `openai` (any OpenAI-compatible
    /// endpoint); unset turns it off. See [`crate::ai`].
    pub ai_provider: Option<String>,
    /// The API key for `ai_provider`; it never leaves the server.
    pub ai_api_key: Option<String>,
    /// Model; default `claude-opus-5-5` on anthropic, required on openai.
    pub ai_model: Option<String>,
    /// Endpoint, for a gateway or a local model.
    pub ai_base_url: Option<String>,
    /// Output tokens per answer.
    pub ai_max_tokens: Option<u32>,
    /// Claude API effort (`low` .. `max`); default the model's.
    pub ai_effort: Option<String>,
    pub studio_dir: Option<PathBuf>,
    pub compaction_interval_secs: Option<u64>,
    pub compaction_memory_threshold_percent: Option<u64>,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    pub slow_query_ms: Option<u64>,
    pub allow_drop_collection: Option<bool>,
    pub studio_enabled: Option<bool>,
    pub quiet: Option<bool>,
    pub allowed_ips: Option<Vec<String>>,
}

impl TomlConfig {
    /// Overlay environment variables onto this config.
    /// Any `TORQUE_*` env var that is set will overwrite the corresponding field.
    fn merge_env(&mut self) {
        if let Ok(v) = std::env::var("TORQUE_API_KEY") {
            self.api_key = Some(v);
        }
        if let Ok(v) = std::env::var("TORQUE_API_ADDR") {
            self.api_addr = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_API_ADDR: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_INGEST_ADDR") {
            self.ingest_addr = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_INGEST_ADDR: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_DATA_DIR") {
            self.data_dir = Some(PathBuf::from(v));
        }
        if let Ok(v) = std::env::var("TORQUE_LOG_LEVEL") {
            self.log_level = Some(v);
        }
        if let Ok(v) = std::env::var("TORQUE_LOG_DIR") {
            self.log_dir = Some(PathBuf::from(v));
        }
        if let Ok(v) = std::env::var("TORQUE_CORS_ORIGINS") {
            self.cors_origins = Some(v.split(',').map(|s| s.trim().to_string()).collect());
        }
        if let Ok(v) = std::env::var("TORQUE_INGEST_ALLOW") {
            self.ingest_allow = Some(v.split(',').map(|s| s.trim().to_string()).collect());
        }
        if let Ok(v) = std::env::var("TORQUE_GPU_VRAM_BUDGET_MB") {
            self.gpu_vram_budget_mb = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_GPU_VRAM_BUDGET_MB: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_MAX_VECTOR_MEMORY_GB") {
            self.max_vector_memory_gb = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_MAX_VECTOR_MEMORY_GB: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_MAX_HTTP_BODY_BYTES") {
            self.max_http_body_bytes = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_MAX_HTTP_BODY_BYTES: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_MAX_MEMORY_PERCENT") {
            self.max_memory_percent = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_MAX_MEMORY_PERCENT: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_MAX_MEMORY_BYTES") {
            self.max_memory_bytes = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_MAX_MEMORY_BYTES: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_MAX_INGEST_CONNECTIONS") {
            self.max_ingest_connections = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_MAX_INGEST_CONNECTIONS: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_QUERY_TIMEOUT_MS") {
            self.query_timeout_ms = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_QUERY_TIMEOUT_MS: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_AI_PROVIDER") {
            self.ai_provider = Some(v);
        }
        if let Ok(v) = std::env::var("TORQUE_AI_API_KEY") {
            self.ai_api_key = Some(v);
        }
        if let Ok(v) = std::env::var("TORQUE_AI_MODEL") {
            self.ai_model = Some(v);
        }
        if let Ok(v) = std::env::var("TORQUE_AI_BASE_URL") {
            self.ai_base_url = Some(v);
        }
        if let Ok(v) = std::env::var("TORQUE_AI_MAX_TOKENS") {
            self.ai_max_tokens = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_AI_MAX_TOKENS: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_AI_EFFORT") {
            self.ai_effort = Some(v);
        }
        if let Ok(v) = std::env::var("TORQUE_STUDIO_DIR") {
            self.studio_dir = Some(PathBuf::from(v));
        }
        if let Ok(v) = std::env::var("TORQUE_COMPACTION_INTERVAL_SECS") {
            self.compaction_interval_secs = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_COMPACTION_INTERVAL_SECS: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_COMPACTION_MEMORY_THRESHOLD_PERCENT") {
            self.compaction_memory_threshold_percent = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_COMPACTION_MEMORY_THRESHOLD_PERCENT: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_TLS_CERT") {
            self.tls_cert = Some(v);
        }
        if let Ok(v) = std::env::var("TORQUE_TLS_KEY") {
            self.tls_key = Some(v);
        }
        if let Ok(v) = std::env::var("TORQUE_SLOW_QUERY_MS") {
            self.slow_query_ms = Some(v.parse().unwrap_or_else(|_| panic!("invalid TORQUE_SLOW_QUERY_MS: {v}")));
        }
        if let Ok(v) = std::env::var("TORQUE_ALLOW_DROP_COLLECTION") {
            self.allow_drop_collection = Some(v == "true" || v == "1");
        }
        if let Ok(v) = std::env::var("TORQUE_STUDIO_ENABLED") {
            self.studio_enabled = Some(v == "true" || v == "1");
        }
        if let Ok(v) = std::env::var("TORQUE_QUIET") {
            self.quiet = Some(v == "true" || v == "1");
        }
        if let Ok(v) = std::env::var("TORQUE_ALLOWED_IPS") {
            self.allowed_ips = Some(v.split(',').map(|s| s.trim().to_string()).collect());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    Default,
    Toml,
    Env,
    Arg,
    Generated,
}

impl fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigSource::Default => write!(f, "default"),
            ConfigSource::Toml => write!(f, "toml"),
            ConfigSource::Env => write!(f, "env"),
            ConfigSource::Arg => write!(f, "arg"),
            ConfigSource::Generated => write!(f, "generated"),
        }
    }
}

/// Generate a random bootstrap API key with `tq_` prefix and 32 hex chars (128-bit entropy).
pub fn generate_api_key() -> String {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("failed to generate random API key");
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    format!("tq_{hex}")
}

#[derive(Debug, Clone)]
pub struct ConfigSources {
    pub api_addr: ConfigSource,
    pub ingest_addr: ConfigSource,
    pub data_dir: ConfigSource,
    pub memory: ConfigSource,
    pub api_key: ConfigSource,
    pub config_file: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub api_addr: SocketAddr,
    pub ingest_addr: SocketAddr,
    pub api_key: String,
    pub data_dir: PathBuf,
    pub log_level: String,
    pub log_dir: Option<PathBuf>,
    pub cors_origins: Vec<String>,
    pub ingest_allow: Vec<String>,
    /// GPU VRAM budget for dense bitmaps in bytes. 0 = auto-detect.
    pub gpu_vram_budget_bytes: u64,
    /// Maximum vector memory in bytes.
    pub max_vector_memory_bytes: u64,
    /// Maximum HTTP request body size in bytes.
    pub max_http_body_bytes: u64,
    /// Maximum memory usage in bytes (resolved from percent or absolute).
    pub max_memory_bytes: u64,
    /// Maximum concurrent TCP ingest connections.
    pub max_ingest_connections: usize,
    /// Query timeout in milliseconds.
    pub query_timeout_ms: u64,
    /// Studio's Search Assistant; `None` is off.
    pub ai: Option<std::sync::Arc<crate::ai::AiConfig>>,
    /// Path to pre-built studio dist directory.
    pub studio_dir: Option<PathBuf>,
    /// Compaction interval in seconds (0 = disabled).
    pub compaction_interval_secs: u64,
    /// Memory pressure compaction threshold as percent of max_memory_bytes (0 = disabled).
    pub compaction_memory_threshold_percent: u64,
    /// TLS certificate file path (PEM).
    pub tls_cert: Option<String>,
    /// TLS private key file path (PEM).
    pub tls_key: Option<String>,
    /// Slow query logging threshold in milliseconds (0 = disabled).
    pub slow_query_ms: u64,
    /// Allow DROP COLLECTION via Studio and HTTP API.
    pub allow_drop_collection: bool,
    /// Whether the embedded Studio web UI is enabled.
    pub studio_enabled: bool,
    /// Suppress the startup banner (unattended/cloud deployments).
    pub quiet: bool,
    /// HTTP client IP allowlist as a comma-separated CIDR string.
    /// Empty string = allow all (default).
    pub allowed_ips: String,
}

impl Config {
    /// Build config with three-layer resolution: CLI args > env vars > TOML > defaults.
    /// Returns the resolved config and the source of each tracked setting.
    #[cfg(feature = "native")]
    pub fn load(cli: CliArgs) -> Result<(Self, ConfigSources), String> {
        // --- Layer 1: Load TOML config file ---
        let config_file;
        let mut toml_cfg = if let Some(ref path) = cli.config {
            let contents = std::fs::read_to_string(path)
                .map_err(|e| format!("Failed to read config file `{}`: {e}", path.display()))?;
            config_file = Some(path.display().to_string());
            toml::from_str::<TomlConfig>(&contents)
                .map_err(|e| format!("Failed to parse config file: {e}"))?
        } else {
            // Auto-detect torque.toml in CWD
            let auto_path = PathBuf::from("torque.toml");
            if auto_path.exists() {
                let contents = std::fs::read_to_string(&auto_path)
                    .map_err(|e| format!("Failed to read torque.toml: {e}"))?;
                config_file = Some("torque.toml".to_string());
                toml::from_str::<TomlConfig>(&contents)
                    .map_err(|e| format!("Failed to parse torque.toml: {e}"))?
            } else {
                config_file = None;
                TomlConfig::default()
            }
        };

        // Init source tracking: Toml if field came from file, Default otherwise
        let toml_source = if config_file.is_some() {
            ConfigSource::Toml
        } else {
            ConfigSource::Default
        };
        let mut src_api_addr = if toml_cfg.api_addr.is_some() { toml_source } else { ConfigSource::Default };
        let mut src_ingest_addr = if toml_cfg.ingest_addr.is_some() { toml_source } else { ConfigSource::Default };
        let mut src_data_dir = if toml_cfg.data_dir.is_some() { toml_source } else { ConfigSource::Default };
        let mut src_memory = if toml_cfg.max_memory_bytes.is_some() || toml_cfg.max_memory_percent.is_some() {
            toml_source
        } else {
            ConfigSource::Default
        };
        let mut src_api_key = if toml_cfg.api_key.is_some() { toml_source } else { ConfigSource::Default };

        // --- Layer 2: Overlay env vars ---
        // Track which env vars are set before merging
        let env_api_addr = std::env::var("TORQUE_API_ADDR").is_ok();
        let env_ingest_addr = std::env::var("TORQUE_INGEST_ADDR").is_ok();
        let env_data_dir = std::env::var("TORQUE_DATA_DIR").is_ok();
        let env_memory = std::env::var("TORQUE_MAX_MEMORY_BYTES").is_ok()
            || std::env::var("TORQUE_MAX_MEMORY_PERCENT").is_ok();
        let env_api_key = std::env::var("TORQUE_API_KEY").is_ok();

        toml_cfg.merge_env();

        if env_api_addr { src_api_addr = ConfigSource::Env; }
        if env_ingest_addr { src_ingest_addr = ConfigSource::Env; }
        if env_data_dir { src_data_dir = ConfigSource::Env; }
        if env_memory { src_memory = ConfigSource::Env; }
        if env_api_key { src_api_key = ConfigSource::Env; }

        // --- Layer 3: Overlay CLI args (highest priority) ---
        if cli.api_addr.is_some() { src_api_addr = ConfigSource::Arg; }
        if cli.ingest_addr.is_some() { src_ingest_addr = ConfigSource::Arg; }
        if cli.data_dir.is_some() { src_data_dir = ConfigSource::Arg; }
        if cli.max_memory_bytes.is_some() || cli.max_memory_percent.is_some() {
            src_memory = ConfigSource::Arg;
        }
        if cli.api_key.is_some() { src_api_key = ConfigSource::Arg; }

        // --- Resolve final values: CLI > env (already in toml_cfg) > defaults ---
        let api_key = match cli.api_key.or(toml_cfg.api_key) {
            Some(key) => key,
            None => {
                src_api_key = ConfigSource::Generated;
                generate_api_key()
            }
        };

        let api_addr = cli
            .api_addr
            .or(toml_cfg.api_addr)
            .unwrap_or_else(|| DEFAULT_API_ADDR.parse().expect("valid default API addr"));

        let ingest_addr = cli
            .ingest_addr
            .or(toml_cfg.ingest_addr)
            .unwrap_or_else(|| DEFAULT_INGEST_ADDR.parse().expect("valid default ingest addr"));

        let data_dir = cli
            .data_dir
            .or(toml_cfg.data_dir)
            .unwrap_or_else(|| PathBuf::from("./data"));

        let log_level = cli
            .log_level
            .or(toml_cfg.log_level)
            .unwrap_or_else(|| "info".into());

        let log_dir = cli.log_dir.or(toml_cfg.log_dir);

        let cors_origins = if let Some(origins) = cli.cors_origins {
            origins.split(',').map(|s| s.trim().to_string()).collect()
        } else {
            toml_cfg
                .cors_origins
                .unwrap_or_else(|| vec!["*".to_string()])
        };

        let ingest_allow = toml_cfg.ingest_allow.unwrap_or_default();

        let gpu_vram_budget_mb = cli
            .gpu_vram_budget_mb
            .or(toml_cfg.gpu_vram_budget_mb)
            .unwrap_or(0);

        let max_vector_memory_gb = cli
            .max_vector_memory_gb
            .or(toml_cfg.max_vector_memory_gb)
            .unwrap_or(16);

        let max_http_body_bytes = cli
            .max_http_body_bytes
            .or(toml_cfg.max_http_body_bytes)
            .unwrap_or(usize::MAX as u64); // No practical limit — TQBF uploads can be arbitrarily large

        let max_ingest_connections = cli
            .max_ingest_connections
            .or(toml_cfg.max_ingest_connections)
            .unwrap_or(64);

        let query_timeout_ms = cli
            .query_timeout_ms
            .or(toml_cfg.query_timeout_ms)
            .unwrap_or(10_000);

        // Resolve memory limit
        let max_memory_bytes = if let Some(bytes) = cli.max_memory_bytes.or(toml_cfg.max_memory_bytes) {
            bytes
        } else {
            let percent = cli
                .max_memory_percent
                .or(toml_cfg.max_memory_percent)
                .unwrap_or(90);
            let total_ram = crate::memory::total_memory_bytes();
            total_ram * percent / 100
        };

        let ai = crate::ai::AiConfig::resolve(crate::ai::AiSettings {
            provider: toml_cfg.ai_provider,
            api_key: toml_cfg.ai_api_key,
            model: toml_cfg.ai_model,
            base_url: toml_cfg.ai_base_url,
            max_tokens: toml_cfg.ai_max_tokens,
            effort: toml_cfg.ai_effort,
        })?
        .map(std::sync::Arc::new);

        let studio_dir = cli.studio_dir.or(toml_cfg.studio_dir);

        let compaction_interval_secs = cli
            .compaction_interval_secs
            .or(toml_cfg.compaction_interval_secs)
            .unwrap_or(1800);

        let compaction_memory_threshold_percent = cli
            .compaction_memory_threshold_percent
            .or(toml_cfg.compaction_memory_threshold_percent)
            .unwrap_or(80);

        #[cfg(feature = "tls")]
        let tls_cert = cli.tls_cert.or(toml_cfg.tls_cert);
        #[cfg(not(feature = "tls"))]
        let tls_cert: Option<String> = toml_cfg.tls_cert;

        #[cfg(feature = "tls")]
        let tls_key = cli.tls_key.or(toml_cfg.tls_key);
        #[cfg(not(feature = "tls"))]
        let tls_key: Option<String> = toml_cfg.tls_key;

        let slow_query_ms = cli.slow_query_ms.or(toml_cfg.slow_query_ms).unwrap_or(0);

        // CLI flag is a one-way override: --allow-drop-collection forces enable.
        // To disable, set `allow_drop_collection = false` in torque.toml
        // or `TORQUE_ALLOW_DROP_COLLECTION=false`. Default is enabled.
        let allow_drop_collection = if cli.allow_drop_collection {
            true
        } else {
            toml_cfg.allow_drop_collection.unwrap_or(true)
        };

        // --no-studio is a one-way override: forces disable. To re-enable,
        // set `studio_enabled = true` in torque.toml or TORQUE_STUDIO_ENABLED=true.
        let studio_enabled = if cli.no_studio {
            false
        } else {
            toml_cfg.studio_enabled.unwrap_or(true)
        };

        // Suppress the startup banner in unattended/cloud deployments (CLI wins,
        // then TOML/TORQUE_QUIET). The banner prints the API key.
        let quiet = cli.quiet || toml_cfg.quiet.unwrap_or(false);

        // HTTP IP allowlist. CLI wins, then TOML (Vec<String>), else empty (allow-all).
        let allowed_ips = if let Some(s) = cli.allowed_ips {
            s
        } else {
            toml_cfg.allowed_ips.map(|v| v.join(",")).unwrap_or_default()
        };

        let sources = ConfigSources {
            api_addr: src_api_addr,
            ingest_addr: src_ingest_addr,
            data_dir: src_data_dir,
            memory: src_memory,
            api_key: src_api_key,
            config_file,
        };

        let config = Config {
            api_addr,
            ingest_addr,
            api_key,
            data_dir,
            log_level,
            log_dir,
            cors_origins,
            ingest_allow,
            gpu_vram_budget_bytes: gpu_vram_budget_mb * 1024 * 1024,
            max_vector_memory_bytes: max_vector_memory_gb * 1024 * 1024 * 1024,
            max_http_body_bytes,
            max_memory_bytes,
            max_ingest_connections,
            query_timeout_ms,
            ai,
            studio_dir,
            compaction_interval_secs,
            compaction_memory_threshold_percent,
            tls_cert,
            tls_key,
            slow_query_ms,
            allow_drop_collection,
            studio_enabled,
            quiet,
            allowed_ips,
        };

        Ok((config, sources))
    }

}
