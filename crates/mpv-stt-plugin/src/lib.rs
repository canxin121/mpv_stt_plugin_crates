pub mod audio;
pub mod config;
pub mod ffi;
pub mod plugin;
pub mod process;
pub mod stt;
pub mod subtitle_manager;
pub mod translate;

pub use audio::AudioExtractor;
pub use config::{Config, InferenceDevice};
pub use mpv_stt_common::{MpvSttError, Result};
pub use mpv_stt_crypto::{AuthToken, EncryptionKey};
pub use mpv_stt_srt::{SrtFile, SubtitleEntry};
#[cfg(any(feature = "stt_local_cpu", feature = "stt_local_cuda"))]
pub use stt::LocalModelConfig;
#[cfg(feature = "stt_remote_http")]
pub use stt::RemoteSttConfig;
pub use stt::{ActiveBackend as SttActiveBackend, SttBackend, SttRunner};
pub use subtitle_manager::SubtitleManager;
pub use translate::{Translator, TranslatorConfig};
