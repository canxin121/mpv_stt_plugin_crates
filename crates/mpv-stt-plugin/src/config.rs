use directories::BaseDirs;
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use log::warn;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

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
    pub local_whisper: Option<SttLocalWhisperConfig>,
    pub remote_http: Option<SttRemoteHttpConfig>,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            local_whisper: Some(SttLocalWhisperConfig::default()),
            remote_http: Some(SttRemoteHttpConfig::default()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttLocalWhisperConfig {
    pub model_path: String,
    pub threads: u8,
    pub language: String,
    pub gpu_device: i32,
    pub flash_attn: bool,
    pub timeout_ms: u64,
}

impl Default for SttLocalWhisperConfig {
    fn default() -> Self {
        Self {
            model_path: "ggml-base.bin".to_string(),
            threads: 8,
            language: "en".to_string(),
            gpu_device: 0,
            flash_attn: false,
            timeout_ms: 120_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttRemoteHttpConfig {
    pub server_addr: String,
    pub timeout_ms: u64,
    pub max_retry: usize,
    /// Enable Opus compression to reduce network payload size.
    pub use_opus: bool,
    pub enable_encryption: bool,
    pub encryption_key: String,
    pub auth_secret: String,
}

impl Default for SttRemoteHttpConfig {
    fn default() -> Self {
        Self {
            server_addr: "http://127.0.0.1:9000".to_string(),
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
pub struct TranslateConfig {
    pub from_lang: String,
    pub to_lang: String,
    pub concurrency: usize,
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            from_lang: "en".to_string(),
            to_lang: "zh".to_string(),
            concurrency: 4,
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
