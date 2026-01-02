use super::{BackendKind, SttBackend, SttDeviceNotice};
use crate::config::InferenceDevice;
use hound::{SampleFormat, WavReader};
use log::{debug, info, trace, warn};
use mpv_stt_common::{MpvSttError, Result};
use mpv_stt_srt::{SrtFile, SubtitleEntry, Timestamp};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use whisper_rs::{
    self, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperError,
};

const EXPECTED_SAMPLE_RATE: u32 = 16_000;
const EXPECTED_CHANNELS: u16 = 1;

#[cfg(feature = "stt_local_cuda")]
const FEATURE_DEVICE: InferenceDevice = InferenceDevice::CUDA;
#[cfg(feature = "stt_local_cpu")]
const FEATURE_DEVICE: InferenceDevice = InferenceDevice::CPU;

#[derive(Clone)]
pub struct LocalModelConfig {
    pub model_path: String,
    pub threads: u8,
    pub language: String,
    pub gpu_device: i32,
    pub flash_attn: bool,
    pub timeout_ms: u64,
}

impl Default for LocalModelConfig {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            threads: 8,
            language: "auto".to_string(),
            gpu_device: 0,
            flash_attn: false,
            timeout_ms: 120_000,
        }
    }
}

impl LocalModelConfig {
    pub fn new(model_path: String) -> Self {
        Self {
            model_path,
            ..Default::default()
        }
    }

    pub fn with_threads(mut self, threads: u8) -> Self {
        self.threads = threads;
        self
    }

    pub fn with_language(mut self, language: String) -> Self {
        self.language = language;
        self
    }

    pub fn with_gpu_device(mut self, device: i32) -> Self {
        self.gpu_device = device;
        self
    }

    pub fn with_flash_attn(mut self, enabled: bool) -> Self {
        self.flash_attn = enabled;
        self
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }
}

pub struct LocalWhisperBackend {
    config: LocalModelConfig,
    ctx: Option<WhisperContext>,
    active_device: Option<InferenceDevice>,
    pending_device_notice: Option<SttDeviceNotice>,
    cancel_generation: Arc<AtomicU64>,
}

impl SttBackend for LocalWhisperBackend {
    fn kind(&self) -> BackendKind {
        #[cfg(feature = "stt_local_cuda")]
        return BackendKind::LocalModelCuda;

        #[cfg(feature = "stt_local_cpu")]
        return BackendKind::LocalModelCpu;
    }

    fn transcribe<P: AsRef<Path>>(
        &mut self,
        audio_path: P,
        output_prefix: P,
        duration_ms: u64,
    ) -> Result<()> {
        self.transcribe_impl(audio_path, output_prefix, duration_ms)
    }

    fn cancel_inflight(&self) {
        self.cancel_generation.fetch_add(1, Ordering::Relaxed);
    }

    fn take_device_notice(&mut self) -> Option<SttDeviceNotice> {
        self.pending_device_notice.take()
    }
}

impl LocalWhisperBackend {
    pub fn new(config: LocalModelConfig) -> Self {
        Self {
            config,
            ctx: None,
            active_device: None,
            pending_device_notice: None,
            cancel_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    fn build_context_params_for_device(
        &self,
        effective_device: InferenceDevice,
        reason: &str,
    ) -> WhisperContextParameters<'static> {
        let mut params = WhisperContextParameters::default();
        let use_gpu = effective_device.is_gpu();

        info!(
            "STT inference device: {} (reason: {}, gpu_device: {})",
            effective_device, reason, self.config.gpu_device
        );

        params
            .use_gpu(use_gpu)
            .gpu_device(self.config.gpu_device)
            .flash_attn(self.config.flash_attn);

        params
    }

    fn ensure_context_for_device(&mut self, device: InferenceDevice, reason: &str) -> Result<()> {
        if self.ctx.is_some() && self.active_device == Some(device) {
            return Ok(());
        }

        let params = self.build_context_params_for_device(device, reason);
        let ctx = WhisperContext::new_with_params(&self.config.model_path, params)
            .map_err(|e| stt_error("Failed to load model", e))?;

        self.ctx = Some(ctx);
        self.active_device = Some(device);
        self.pending_device_notice = Some(SttDeviceNotice {
            requested: FEATURE_DEVICE,
            effective: device,
            reason: reason.to_string(),
            gpu_device: self.config.gpu_device,
        });
        Ok(())
    }

    fn ensure_context(&mut self) -> Result<()> {
        if self.ctx.is_some() {
            return Ok(());
        }

        if self.config.model_path.trim().is_empty() {
            return Err(MpvSttError::SttFailed("Model path is empty".to_string()));
        }

        if let Err(err) =
            self.ensure_context_for_device(FEATURE_DEVICE, "feature-selected local_model")
        {
            if FEATURE_DEVICE.is_gpu() {
                warn!(
                    "STT context init failed on {} ({}); falling back to CPU",
                    FEATURE_DEVICE, err
                );
                self.ctx = None;
                self.active_device = None;
                return self.ensure_context_for_device(
                    InferenceDevice::CPU,
                    "fallback after gpu init failure",
                );
            }
            return Err(err);
        }

        Ok(())
    }

    fn load_audio_samples<P: AsRef<Path>>(&self, audio_path: P) -> Result<Vec<f32>> {
        let mut reader = WavReader::open(audio_path.as_ref())
            .map_err(|e| MpvSttError::SttFailed(format!("Failed to read WAV: {}", e)))?;
        let spec = reader.spec();

        if spec.channels != EXPECTED_CHANNELS
            || spec.sample_rate != EXPECTED_SAMPLE_RATE
            || spec.bits_per_sample != 16
            || spec.sample_format != SampleFormat::Int
        {
            return Err(MpvSttError::SttFailed(format!(
                "Unexpected WAV format: channels={}, sample_rate={}, bits_per_sample={}, format={:?}",
                spec.channels, spec.sample_rate, spec.bits_per_sample, spec.sample_format
            )));
        }

        let samples: Vec<i16> = reader
            .samples::<i16>()
            .collect::<std::result::Result<Vec<i16>, _>>()
            .map_err(|e| MpvSttError::SttFailed(format!("Failed to read WAV samples: {}", e)))?;

        let mut float_samples = vec![0f32; samples.len()];
        whisper_rs::convert_integer_to_float_audio(&samples, &mut float_samples)
            .map_err(|e| stt_error("Failed to convert audio", e))?;

        Ok(float_samples)
    }

    fn build_params<'a>(&'a self, duration_ms: u64) -> FullParams<'a, 'a> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 5 });
        params.set_n_threads(self.config.threads as i32);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_print_special(false);
        params.set_no_timestamps(false);
        params.set_translate(false);

        if self.config.language.trim().eq_ignore_ascii_case("auto") {
            params.set_detect_language(true);
            params.set_language(None);
        } else {
            params.set_detect_language(false);
            params.set_language(Some(self.config.language.as_str()));
        }

        let dur_i32 = i32::try_from(duration_ms).unwrap_or(i32::MAX);
        params.set_offset_ms(0);
        params.set_duration_ms(dur_i32);
        params
    }

    fn detect_language(
        &self,
        state: &mut whisper_rs::WhisperState,
        audio: &[f32],
    ) -> Result<String> {
        // Prepare mel spectrogram for language detection.
        state
            .pcm_to_mel(audio, self.config.threads as usize)
            .map_err(|e| stt_error("Failed to prepare mel for language detection", e))?;

        let (lang_id, _probs) = state
            .lang_detect(0, self.config.threads as usize)
            .map_err(|e| stt_error("Language detection failed", e))?;

        if let Some(lang) = whisper_rs::get_lang_str(lang_id) {
            Ok(lang.to_string())
        } else {
            Err(MpvSttError::SttFailed(format!(
                "Unknown language id detected: {}",
                lang_id
            )))
        }
    }

    fn write_srt<P: AsRef<Path>>(&self, output_prefix: P, segments: &[SegmentData]) -> Result<()> {
        let mut srt_file = SrtFile::new();
        for (idx, segment) in segments.iter().enumerate() {
            srt_file.append_entry(SubtitleEntry {
                index: (idx + 1) as u32,
                start_time: Timestamp::from_milliseconds(segment.start_ms),
                end_time: Timestamp::from_milliseconds(segment.end_ms),
                text: segment.text.clone(),
            });
        }

        let output_path = PathBuf::from(output_prefix.as_ref()).with_extension("srt");
        srt_file.save(&output_path)?;
        Ok(())
    }

    fn transcribe_impl<P: AsRef<Path>>(
        &mut self,
        audio_path: P,
        output_prefix: P,
        duration_ms: u64,
    ) -> Result<()> {
        self.ensure_context()?;

        let audio_str = audio_path
            .as_ref()
            .to_str()
            .ok_or_else(|| MpvSttError::InvalidPath("Invalid audio path".to_string()))?;

        trace!(
            "Running local model STT on {} (duration: {}ms, language: {})",
            audio_str, duration_ms, self.config.language
        );

        let audio = self.load_audio_samples(&audio_path)?;
        if audio.is_empty() {
            return Err(MpvSttError::SttFailed("Audio buffer is empty".to_string()));
        }

        let audio_duration_ms = (audio.len() as u64)
            .saturating_mul(1000)
            .saturating_div(EXPECTED_SAMPLE_RATE as u64);
        if audio_duration_ms == 0 {
            return Err(MpvSttError::SttFailed("Audio duration is zero".to_string()));
        }
        let effective_duration_ms = audio_duration_ms.min(duration_ms);
        if effective_duration_ms != duration_ms {
            debug!(
                "STT duration clamp: requested={}ms, actual={}ms",
                duration_ms, audio_duration_ms
            );
        }

        let segments = match self.run_inference(&audio, effective_duration_ms) {
            Ok(segments) => Ok(segments),
            Err(err) => {
                if matches!(err, MpvSttError::SttCancelled) {
                    return Err(err);
                }
                if self.active_device.map_or(false, |device| device.is_gpu()) {
                    let failed_device = self.active_device.unwrap_or(InferenceDevice::CPU);
                    warn!(
                        "STT inference failed on {} ({}); retrying on CPU",
                        failed_device, err
                    );
                    self.ctx = None;
                    self.active_device = None;
                    self.ensure_context_for_device(
                        InferenceDevice::CPU,
                        "fallback after gpu inference failure",
                    )?;
                    self.run_inference(&audio, effective_duration_ms)
                } else {
                    Err(err)
                }
            }
        }?;
        self.write_srt(output_prefix, &segments)?;

        debug!("Local model STT completed successfully");
        Ok(())
    }
}

impl LocalWhisperBackend {
    fn run_inference(&self, audio: &[f32], duration_ms: u64) -> Result<Vec<SegmentData>> {
        let run_generation = self.cancel_generation.load(Ordering::Relaxed);
        let ctx = self
            .ctx
            .as_ref()
            .ok_or_else(|| MpvSttError::SttFailed("STT context not initialized".to_string()))?;
        let mut state = ctx
            .create_state()
            .map_err(|e| stt_error("Failed to create state", e))?;

        let mut params = self.build_params(duration_ms);
        // whisper.cpp 在 detect_language 模式下偶现“检测到语言但无分段”的情况；
        // 为避免空结果，先单独做语言检测，再用检测到的语言跑一次完整转录。
        let mut detected_lang: Option<String> = None;
        if self.config.language.trim().eq_ignore_ascii_case("auto") {
            match self.detect_language(&mut state, audio) {
                Ok(lang) => {
                    detected_lang = Some(lang);
                    params.set_detect_language(false);
                }
                Err(e) => {
                    warn!("Language detection failed, fallback to auto: {}", e);
                    params.set_detect_language(true);
                    params.set_language(None);
                }
            }
        }

        if let Some(ref lang) = detected_lang {
            params.set_language(Some(lang.as_str()));
            info!("Detected language: {}", lang);
        };

        let abort_ctx = Box::new(AbortContext {
            generation: Arc::clone(&self.cancel_generation),
            run_generation,
        });
        let abort_ptr = Box::into_raw(abort_ctx);
        let _abort_guard = AbortGuard(abort_ptr);
        unsafe {
            params.set_abort_callback(Some(whisper_abort_callback));
            params.set_abort_callback_user_data(abort_ptr as *mut c_void);
        }

        if let Err(err) = state.full(params, audio) {
            if self.cancel_generation.load(Ordering::Relaxed) != run_generation {
                return Err(MpvSttError::SttCancelled);
            }
            return Err(stt_error("STT inference failed", err));
        }

        if self.cancel_generation.load(Ordering::Relaxed) != run_generation {
            return Err(MpvSttError::SttCancelled);
        }

        collect_segments(&state)
    }
}

struct AbortContext {
    generation: Arc<AtomicU64>,
    run_generation: u64,
}

struct AbortGuard(*mut AbortContext);

impl Drop for AbortGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                drop(Box::from_raw(self.0));
            }
        }
    }
}

unsafe extern "C" fn whisper_abort_callback(user_data: *mut c_void) -> bool {
    if user_data.is_null() {
        return false;
    }
    let ctx = unsafe { &*(user_data as *const AbortContext) };
    ctx.generation.load(Ordering::Relaxed) != ctx.run_generation
}

struct SegmentData {
    start_ms: u32,
    end_ms: u32,
    text: String,
}

fn collect_segments(state: &whisper_rs::WhisperState) -> Result<Vec<SegmentData>> {
    let mut segments = Vec::new();
    for segment in state.as_iter() {
        let start_ms = timestamp_to_millis(segment.start_timestamp());
        let end_ms = timestamp_to_millis(segment.end_timestamp());
        if end_ms <= start_ms {
            continue;
        }
        let text = segment
            .to_str_lossy()
            .map_err(|e| stt_error("Failed to read segment text", e))?
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        segments.push(SegmentData {
            start_ms,
            end_ms,
            text,
        });
    }
    Ok(segments)
}

fn timestamp_to_millis(timestamp_cs: i64) -> u32 {
    let millis = timestamp_cs.saturating_mul(10);
    if millis < 0 {
        0
    } else if millis > u32::MAX as i64 {
        u32::MAX
    } else {
        millis as u32
    }
}

fn stt_error(context: &str, err: WhisperError) -> MpvSttError {
    MpvSttError::SttFailed(format!("{}: {}", context, err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builder() {
        let config = LocalModelConfig::new("/path/to/model".to_string())
            .with_threads(4)
            .with_language("ja".to_string())
            .with_gpu_device(1)
            .with_flash_attn(true)
            .with_timeout_ms(42_000);

        assert_eq!(config.model_path, "/path/to/model");
        assert_eq!(config.threads, 4);
        assert_eq!(config.language, "ja");
        assert_eq!(config.gpu_device, 1);
        assert!(config.flash_attn);
        assert_eq!(config.timeout_ms, 42_000);
    }
}
