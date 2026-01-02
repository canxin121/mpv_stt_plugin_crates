use crate::config::InferenceDevice;
use mpv_stt_common::Result;
use std::path::Path;

/// Enumerates available speech-to-text backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    LocalModelCpu,
    LocalModelCuda,
    RemoteHttp,
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendKind::LocalModelCpu => write!(f, "local-model-cpu"),
            BackendKind::LocalModelCuda => write!(f, "local-model-cuda"),
            BackendKind::RemoteHttp => write!(f, "remote-http"),
        }
    }
}

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

    /// Optional notice about the effective device used (for UI).
    fn take_device_notice(&mut self) -> Option<SttDeviceNotice>;
}

#[derive(Debug, Clone)]
pub struct SttDeviceNotice {
    pub requested: InferenceDevice,
    pub effective: InferenceDevice,
    pub reason: String,
    pub gpu_device: i32,
}

// Enforce exactly one backend at compile time.
#[cfg(not(any(
    feature = "stt_local_cpu",
    feature = "stt_local_cuda",
    feature = "stt_remote_http"
)))]
compile_error!(
    "No STT backend selected. Enable exactly one of: stt_local_cpu, stt_local_cuda, stt_remote_http"
);

#[cfg(any(
    all(feature = "stt_local_cpu", feature = "stt_local_cuda"),
    all(feature = "stt_local_cpu", feature = "stt_remote_http"),
    all(feature = "stt_local_cuda", feature = "stt_remote_http")
))]
compile_error!("Cannot enable multiple STT backends simultaneously");

#[cfg(all(target_os = "android", feature = "stt_local_cuda"))]
compile_error!("Android does not support the stt_local_cuda backend");

// Backend modules
#[cfg(any(feature = "stt_local_cpu", feature = "stt_local_cuda"))]
mod local_whisper;

#[cfg(feature = "stt_remote_http")]
mod remote_http;

// Active backend type alias
#[cfg(feature = "stt_local_cpu")]
pub use local_whisper::LocalWhisperBackend as ActiveBackend;

#[cfg(feature = "stt_local_cuda")]
pub use local_whisper::LocalWhisperBackend as ActiveBackend;

#[cfg(feature = "stt_remote_http")]
pub use remote_http::RemoteHttpBackend as ActiveBackend;

// Config exports
#[cfg(any(feature = "stt_local_cpu", feature = "stt_local_cuda"))]
pub use local_whisper::LocalModelConfig;

#[cfg(feature = "stt_remote_http")]
pub use remote_http::RemoteSttConfig;

// Convenience type alias (ergonomic only)
#[cfg(any(feature = "stt_local_cpu", feature = "stt_local_cuda"))]
pub type SttRunner = local_whisper::LocalWhisperBackend;

#[cfg(feature = "stt_remote_http")]
pub type SttRunner = remote_http::RemoteHttpBackend;
