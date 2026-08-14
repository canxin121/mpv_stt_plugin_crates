pub mod audio;
pub mod common;
pub mod config;
pub mod crypto;
pub mod ffi;
pub mod plugin;
pub mod process;
pub mod stt;
pub mod srt;
pub mod subtitle_manager;
pub mod translate;

pub use audio::AudioExtractor;
pub use config::{
    BackendKind, Config, InferenceDevice, TranslateBackendKind, TranslateLibreTranslateConfig,
};
pub use crate::common::{MpvSttError, Result};
pub use crate::crypto::{AuthToken, EncryptionKey};
pub use crate::srt::{SrtFile, SubtitleEntry};
#[cfg(feature = "stt_ferrum")]
pub use stt::SttFerrumConfig;
#[cfg(feature = "stt_openai")]
pub use stt::SttOpenAiConfig;
pub use stt::{SttBackend, SttRunner};
pub use subtitle_manager::SubtitleManager;
pub use translate::{Translator, TranslatorConfig};
