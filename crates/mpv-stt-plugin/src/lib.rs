pub mod audio;
pub mod config;
pub mod ffi;
pub mod plugin;
pub mod process;
pub mod stt;
pub mod subtitle_manager;
pub mod translate;

pub use audio::AudioExtractor;
pub use config::{
    BackendKind, Config, InferenceDevice, TranslateBackendKind, TranslateLibreTranslateConfig,
};
pub use mpv_stt_common::{MpvSttError, Result};
pub use mpv_stt_crypto::{AuthToken, EncryptionKey};
pub use mpv_stt_srt::{SrtFile, SubtitleEntry};
#[cfg(feature = "stt_ferrum")]
pub use stt::SttFerrumConfig;
#[cfg(feature = "stt_openai")]
pub use stt::SttOpenAiConfig;
pub use stt::{SttBackend, SttRunner};
pub use subtitle_manager::SubtitleManager;
pub use translate::{Translator, TranslatorConfig};
