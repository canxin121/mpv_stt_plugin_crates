use crate::common::Result;
use crate::config::SttConfig;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicU64};

pub use crate::config::BackendKind;

/// Common trait for all speech-to-text backends.
pub trait SttBackend: Send {
    fn kind(&self) -> BackendKind;

    fn transcribe<P: AsRef<Path>>(
        &mut self,
        audio_path: P,
        output_prefix: P,
        duration_ms: u64,
    ) -> Result<()>;

    /// Request cancellation of in-flight work.
    fn cancel_inflight(&self);

    /// Shared generation used by an external event loop to cancel a backend
    /// while `transcribe` is running on a worker thread.
    fn cancellation_generation(&self) -> Arc<AtomicU64>;

    /// Optional notice about the effective device used (for UI).
    fn take_device_notice(&mut self) -> Option<SttDeviceNotice>;
}

#[derive(Debug, Clone)]
pub struct SttDeviceNotice {
    pub requested: crate::config::InferenceDevice,
    pub effective: crate::config::InferenceDevice,
    pub reason: String,
    pub gpu_device: i32,
}

// Backend modules. Both remote backends are compiled in (the plugin is a pure
// remote client); the active one is chosen at runtime via `config.stt.backend`.
#[cfg(feature = "stt_ferrum")]
mod ferrum;

#[cfg(feature = "stt_openai")]
mod openai;

// Config exports
#[cfg(feature = "stt_ferrum")]
pub use ferrum::SttFerrumConfig;

#[cfg(feature = "stt_openai")]
pub use openai::SttOpenAiConfig;

/// Runtime-selected STT backend. Both remote backends are compiled in; which
/// one actually runs is decided from `SttConfig::backend` at startup.
pub enum SttRunner {
    #[cfg(feature = "stt_ferrum")]
    Ferrum(ferrum::FerrumBackend),
    #[cfg(feature = "stt_openai")]
    OpenAi(openai::OpenAiBackend),
}

impl SttRunner {
    /// Build the active backend from the runtime config, matching `cfg.backend`.
    pub fn from_config(cfg: &SttConfig) -> Result<Self> {
        match cfg.backend {
            BackendKind::Ferrum => {
                #[cfg(feature = "stt_ferrum")]
                {
                    let ferrum_cfg = cfg.ferrum.as_ref().ok_or_else(|| {
                        crate::common::MpvSttError::SttFailed(
                            "Missing [stt.ferrum] configuration".to_string(),
                        )
                    })?;
                    Ok(SttRunner::Ferrum(ferrum::FerrumBackend::new(
                        ferrum_cfg.clone(),
                    )?))
                }
                #[cfg(not(feature = "stt_ferrum"))]
                {
                    let _ = cfg;
                    Err(crate::common::MpvSttError::SttFailed(
                        "stt_ferrum feature not enabled".to_string(),
                    ))
                }
            }
            BackendKind::OpenAi => {
                #[cfg(feature = "stt_openai")]
                {
                    let openai_cfg = cfg.openai.as_ref().ok_or_else(|| {
                        crate::common::MpvSttError::SttFailed(
                            "Missing [stt.openai] configuration".to_string(),
                        )
                    })?;
                    Ok(SttRunner::OpenAi(openai::OpenAiBackend::new(
                        openai_cfg.clone(),
                    )?))
                }
                #[cfg(not(feature = "stt_openai"))]
                {
                    let _ = cfg;
                    Err(crate::common::MpvSttError::SttFailed(
                        "stt_openai feature not enabled".to_string(),
                    ))
                }
            }
        }
    }
}

impl SttBackend for SttRunner {
    fn kind(&self) -> BackendKind {
        match self {
            #[cfg(feature = "stt_ferrum")]
            SttRunner::Ferrum(b) => b.kind(),
            #[cfg(feature = "stt_openai")]
            SttRunner::OpenAi(b) => b.kind(),
        }
    }

    fn transcribe<P: AsRef<Path>>(
        &mut self,
        audio_path: P,
        output_prefix: P,
        duration_ms: u64,
    ) -> Result<()> {
        match self {
            #[cfg(feature = "stt_ferrum")]
            SttRunner::Ferrum(b) => b.transcribe(audio_path, output_prefix, duration_ms),
            #[cfg(feature = "stt_openai")]
            SttRunner::OpenAi(b) => b.transcribe(audio_path, output_prefix, duration_ms),
        }
    }

    fn cancel_inflight(&self) {
        match self {
            #[cfg(feature = "stt_ferrum")]
            SttRunner::Ferrum(b) => b.cancel_inflight(),
            #[cfg(feature = "stt_openai")]
            SttRunner::OpenAi(b) => b.cancel_inflight(),
        }
    }

    fn cancellation_generation(&self) -> Arc<AtomicU64> {
        match self {
            #[cfg(feature = "stt_ferrum")]
            SttRunner::Ferrum(b) => b.cancellation_generation(),
            #[cfg(feature = "stt_openai")]
            SttRunner::OpenAi(b) => b.cancellation_generation(),
        }
    }

    fn take_device_notice(&mut self) -> Option<SttDeviceNotice> {
        match self {
            #[cfg(feature = "stt_ferrum")]
            SttRunner::Ferrum(b) => b.take_device_notice(),
            #[cfg(feature = "stt_openai")]
            SttRunner::OpenAi(b) => b.take_device_notice(),
        }
    }
}
