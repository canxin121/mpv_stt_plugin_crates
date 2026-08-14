use directories::BaseDirs;
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use log::warn;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// Which remote STT backend to use at runtime. The plugin is a pure remote
/// client: both backends are compiled in, and this key selects the active one
/// (no compile-time feature exclusivity).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Ferrum,
    OpenAi,
}

impl Default for BackendKind {
    fn default() -> Self {
        BackendKind::OpenAi
    }
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            BackendKind::Ferrum => "ferrum",
            BackendKind::OpenAi => "openai",
        };
        write!(f, "{label}")
    }
}

/// Which translation protocol to use at runtime. Both protocols are compiled
/// in (no feature exclusivity); this key selects the active one. Mirrors
/// `BackendKind` for the STT backends.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TranslateBackendKind {
    DeepL,
    LibreTranslate,
}

impl Default for TranslateBackendKind {
    fn default() -> Self {
        TranslateBackendKind::DeepL
    }
}

impl std::fmt::Display for TranslateBackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            TranslateBackendKind::DeepL => "deepl",
            TranslateBackendKind::LibreTranslate => "libretranslate",
        };
        write!(f, "{label}")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InferenceDevice {
    CPU,
    CUDA,
}

impl Default for InferenceDevice {
    fn default() -> Self {
        InferenceDevice::CPU
    }
}

impl InferenceDevice {
    pub fn is_gpu(self) -> bool {
        matches!(self, InferenceDevice::CUDA)
    }

    pub fn from_i32(value: i32) -> Self {
        match value {
            1 => InferenceDevice::CUDA,
            _ => InferenceDevice::CPU,
        }
    }
}

impl fmt::Display for InferenceDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            InferenceDevice::CPU => "cpu",
            InferenceDevice::CUDA => "cuda",
        };
        write!(f, "{label}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub stt: SttConfig,
    pub translate: TranslateConfig,
    pub chunk: ChunkConfig,
    pub timeout: TimeoutConfig,
    pub playback: PlaybackConfig,
    pub prefetch: PrefetchConfig,
    pub network: NetworkConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stt: SttConfig::default(),
            translate: TranslateConfig::default(),
            chunk: ChunkConfig::default(),
            timeout: TimeoutConfig::default(),
            playback: PlaybackConfig::default(),
            prefetch: PrefetchConfig::default(),
            network: NetworkConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttConfig {
    /// Runtime backend selector: which of the compiled remote backends is active.
    pub backend: BackendKind,
    pub ferrum: Option<SttFerrumConfig>,
    pub openai: Option<SttOpenAiConfig>,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            backend: BackendKind::default(),
            ferrum: Some(SttFerrumConfig::default()),
            openai: Some(SttOpenAiConfig::default()),
        }
    }
}

/// Custom "ferrum" protocol backend: postcard-free raw HTTP against a
/// ferrum-capable server (e.g. subtitle-gateway's /transcribe endpoint), with
/// optional Opus compression, AES-GCM encryption and token auth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttFerrumConfig {
    pub server_addr: String,
    /// Model id sent via the `x-model` header, e.g. "sensevoice" or "fun-asr-mlt-nano".
    pub model: String,
    /// Optional language hint sent via the `x-language` header (e.g. "ja",
    /// "zh", "en"); `None` = server auto-detects.
    pub language: Option<String>,
    pub timeout_ms: u64,
    pub max_retry: usize,
    /// Enable Opus compression to reduce network payload size.
    pub use_opus: bool,
    pub enable_encryption: bool,
    pub encryption_key: String,
    pub auth_secret: String,
}

impl Default for SttFerrumConfig {
    fn default() -> Self {
        Self {
            server_addr: "http://127.0.0.1:9000".to_string(),
            model: "sensevoice".to_string(),
            language: None,
            timeout_ms: 120_000,
            max_retry: 3,
            use_opus: true,
            enable_encryption: false,
            encryption_key: String::new(),
            auth_secret: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttOpenAiConfig {
    /// Base URL of an OpenAI-compatible transcription server, e.g. http://127.0.0.1:8000.
    pub server_addr: String,
    /// Model id sent in the multipart form, e.g. "sensevoice" or "fun-asr-mlt-nano".
    pub model: String,
    /// Optional language hint (e.g. "ja", "zh", "en").
    pub language: Option<String>,
    /// Optional API key sent as `Authorization: Bearer {key}` for servers that
    /// require auth (e.g. OpenAI-hosted or any key-gated compatible service).
    /// `None` omits the header (local subtitle-gateway needs no key).
    pub api_key: Option<String>,
    pub timeout_ms: u64,
    pub max_retry: usize,
}

impl Default for SttOpenAiConfig {
    fn default() -> Self {
        Self {
            server_addr: "http://127.0.0.1:8000".to_string(),
            model: "sensevoice".to_string(),
            language: None,
            api_key: None,
            timeout_ms: 120_000,
            max_retry: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslateConfig {
    pub from_lang: String,
    pub to_lang: String,
    pub concurrency: usize,
    /// Runtime protocol selector: which of the compiled translation protocols
    /// is active (`deepl` | `libretranslate`).
    pub backend: TranslateBackendKind,
    /// DeepL-compatible translation service base URL (e.g. the subtitle-gateway
    /// gateway at its default port 8000, or an upstream DeepL-compatible API).
    pub server_addr: String,
    /// Optional API key, sent as `Authorization: DeepL-Auth-Key {key}`.
    pub api_key: String,
    /// LibreTranslate backend (only read when `backend = "libretranslate"`).
    pub libretranslate: Option<TranslateLibreTranslateConfig>,
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            from_lang: "en".to_string(),
            to_lang: "zh".to_string(),
            concurrency: 4,
            backend: TranslateBackendKind::default(),
            server_addr: "http://127.0.0.1:8000".to_string(),
            api_key: String::new(),
            libretranslate: Some(TranslateLibreTranslateConfig::default()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslateLibreTranslateConfig {
    /// LibreTranslate-compatible service base URL.
    pub server_addr: String,
    /// Optional API key, sent in the request body as `api_key`.
    pub api_key: String,
}

impl Default for TranslateLibreTranslateConfig {
    fn default() -> Self {
        Self {
            server_addr: "http://127.0.0.1:8000".to_string(),
            api_key: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkConfig {
    pub local_ms: u64,
    pub network_ms: u64,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            local_ms: 15_000,
            network_ms: 15_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutConfig {
    pub ffmpeg_ms: u64,
    pub ffprobe_ms: u64,
    pub stt_ms: u64,
    pub translate_ms: u64,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            ffmpeg_ms: 30_000,
            ffprobe_ms: 10_000,
            stt_ms: 120_000,
            translate_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackConfig {
    pub show_progress: bool,
    pub save_srt: bool,
    pub auto_start: bool,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            show_progress: true,
            save_srt: true,
            auto_start: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefetchConfig {
    pub lookahead_chunks: usize,
}

impl Default for PrefetchConfig {
    fn default() -> Self {
        Self {
            lookahead_chunks: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub demuxer_max_bytes: Option<i64>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            demuxer_max_bytes: None,
        }
    }
}

impl Config {
    pub fn default_config_path() -> Option<PathBuf> {
        let base = BaseDirs::new()?;
        Some(base.config_dir().join("mpv").join("mpv_stt_plugin_rs.toml"))
    }

    pub fn config_path_from_env() -> Option<PathBuf> {
        std::env::var_os("MPV_STT_PLUGIN_RS_CONFIG").map(PathBuf::from)
    }

    pub fn load() -> Self {
        let env_path = Self::config_path_from_env();
        let config_path = env_path.clone().or_else(Self::default_config_path);

        let mut figment = Figment::from(Serialized::defaults(Config::default()));

        if let Some(path) = config_path.as_ref() {
            figment = figment.merge(Toml::file(path));
        }

        // Env should take precedence over file/defaults.
        figment = figment.merge(Env::prefixed("MPV_STT_PLUGIN_RS_"));

        match figment.extract::<Config>() {
            Ok(cfg) => cfg,
            Err(err) => {
                // Logging might not be initialized yet; fall back silently.
                warn!("Failed to load config, using defaults: {err}");
                Config::default()
            }
        }
    }
}
