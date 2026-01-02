use crate::config::InferenceDevice;
use crate::error::{Result, MpvSttError};
use crate::process::run_capture_output_with_stdin;
use mpv_stt_srt::{SrtFile, SubtitleEntry};
use log::{debug, info, trace};
use srtlib::Timestamp;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

const FAST_WHISPER_REPO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../faster-whisper");

#[cfg(feature = "fast_whisper_cuda")]
const FEATURE_DEVICE: InferenceDevice = InferenceDevice::CUDA;
#[cfg(feature = "fast_whisper_cpu")]
const FEATURE_DEVICE: InferenceDevice = InferenceDevice::CPU;

#[derive(serde::Deserialize)]
struct PythonSegment {
    start: f64,
    end: f64,
    text: String,
}

#[derive(serde::Deserialize)]
struct PythonResult {
    segments: Vec<PythonSegment>,
}

pub struct WhisperConfig {
    pub model_path: String,
    pub threads: u8,
    pub language: String,
    pub gpu_device: i32,
    pub flash_attn: bool,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct WhisperDeviceNotice {
    pub requested: InferenceDevice,
    pub effective: InferenceDevice,
    pub reason: String,
    pub gpu_device: i32,
}

impl Default for WhisperConfig {
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

impl WhisperConfig {
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

pub struct WhisperRunner {
    config: WhisperConfig,
    active_device: Option<InferenceDevice>,
    pending_device_notice: Option<WhisperDeviceNotice>,
    cancel_generation: Arc<AtomicU64>,
}

impl WhisperRunner {
    pub fn new(config: WhisperConfig) -> Self {
        Self {
            config,
            active_device: None,
            pending_device_notice: None,
            cancel_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn take_device_notice(&mut self) -> Option<WhisperDeviceNotice> {
        self.pending_device_notice.take()
    }

    pub fn cancel_inflight(&self) {
        self.cancel_generation.fetch_add(1, Ordering::Relaxed);
    }

    fn python_executable() -> String {
        if let Ok(value) = std::env::var("MPV_STT_PLUGIN_RS_PYTHON") {
            if !value.trim().is_empty() {
                return value;
            }
        }

        if cfg!(target_os = "windows") {
            "python".to_string()
        } else {
            "python3".to_string()
        }
    }

    fn build_python_script() -> String {
        // Minimal script to run faster-whisper and return JSON segments.
        r#"
import json
import os
from faster_whisper import WhisperModel

model_path = os.environ["WHISPER_MODEL"]
audio_path = os.environ["WHISPER_AUDIO"]
language = os.environ.get("WHISPER_LANGUAGE", "auto")

device = os.environ.get("WHISPER_DEVICE", "cpu")
compute_type = os.environ.get("WHISPER_COMPUTE_TYPE", "default")

try:
    device_index = int(os.environ.get("WHISPER_DEVICE_INDEX", "0"))
except ValueError:
    device_index = 0

try:
    cpu_threads = int(os.environ.get("WHISPER_CPU_THREADS", "0"))
except ValueError:
    cpu_threads = 0

try:
    beam_size = int(os.environ.get("WHISPER_BEAM_SIZE", "5"))
except ValueError:
    beam_size = 5

model = WhisperModel(
    model_path,
    device=device,
    device_index=device_index,
    compute_type=compute_type,
    cpu_threads=cpu_threads,
)

segments, _info = model.transcribe(
    audio_path,
    language=None if language.strip().lower() in ("", "auto") else language,
    beam_size=beam_size,
)

result = {
    "segments": [
        {"start": seg.start, "end": seg.end, "text": seg.text}
        for seg in segments
    ]
}
print(json.dumps(result, ensure_ascii=False))
"#
        .to_string()
    }

    fn run_python_transcribe<P: AsRef<Path>>(
        &self,
        audio_path: P,
        device: InferenceDevice,
    ) -> Result<PythonResult> {
        if self.config.model_path.trim().is_empty() {
            return Err(MpvSttError::SttFailed(
                "Model path is empty".to_string(),
            ));
        }

        let python = Self::python_executable();
        let script = Self::build_python_script();
        let timeout = Duration::from_millis(self.config.timeout_ms.max(1));

        let mut cmd = Command::new(python);
        cmd.arg("-");

        let language = self.config.language.trim();
        let device_label = match device {
            InferenceDevice::CPU => "cpu",
            InferenceDevice::CUDA => "cuda",
        };

        cmd.env("WHISPER_MODEL", &self.config.model_path)
            .env("WHISPER_AUDIO", audio_path.as_ref())
            .env("WHISPER_DEVICE", device_label)
            .env("WHISPER_DEVICE_INDEX", self.config.gpu_device.to_string())
            .env("WHISPER_LANGUAGE", language)
            .env("WHISPER_CPU_THREADS", self.config.threads.to_string())
            .env(
                "WHISPER_COMPUTE_TYPE",
                std::env::var("MPV_STT_PLUGIN_RS_FAST_WHISPER_COMPUTE_TYPE")
                    .unwrap_or_else(|_| "default".to_string()),
            )
            .env(
                "WHISPER_BEAM_SIZE",
                std::env::var("MPV_STT_PLUGIN_RS_FAST_WHISPER_BEAM_SIZE")
                    .unwrap_or_else(|_| "5".to_string()),
            );

        let sep = if cfg!(target_os = "windows") {
            ";"
        } else {
            ":"
        };
        let python_path = match std::env::var("PYTHONPATH") {
            Ok(existing) if !existing.trim().is_empty() => {
                format!("{}{}{}", existing, sep, FAST_WHISPER_REPO)
            }
            _ => FAST_WHISPER_REPO.to_string(),
        };
        cmd.env("PYTHONPATH", python_path);

        let output =
            run_capture_output_with_stdin(cmd, "faster-whisper", script.as_bytes(), timeout)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MpvSttError::SttFailed(format!(
                "faster-whisper failed: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(stdout.trim()).map_err(|e| {
            MpvSttError::SttFailed(format!("Failed to parse faster-whisper output: {}", e))
        })
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

    /// Run faster-whisper on audio file and generate SRT subtitles
    pub fn transcribe<P: AsRef<Path>>(
        &mut self,
        audio_path: P,
        output_prefix: P,
        _duration_ms: u64,
    ) -> Result<()> {
        let audio_str = audio_path
            .as_ref()
            .to_str()
            .ok_or_else(|| MpvSttError::InvalidPath("Invalid audio path".to_string()))?;

        trace!(
            "Running faster-whisper on {} (language: {})",
            audio_str, self.config.language
        );

        let run_generation = self.cancel_generation.load(Ordering::Relaxed);

        let device = FEATURE_DEVICE;
        let reason = if device.is_gpu() {
            "feature fast_whisper_cuda selected"
        } else {
            "feature fast_whisper_cpu selected"
        };
        info!(
            "Whisper inference device: {} (reason: {}, gpu_device: {})",
            device, reason, self.config.gpu_device
        );
        self.active_device = Some(device);
        self.pending_device_notice = Some(WhisperDeviceNotice {
            requested: device,
            effective: device,
            reason: reason.to_string(),
            gpu_device: self.config.gpu_device,
        });

        let result = self.run_python_transcribe(&audio_path, device)?;

        if self.cancel_generation.load(Ordering::Relaxed) != run_generation {
            return Err(MpvSttError::SttCancelled);
        }

        let segments = collect_segments(&result)?;
        self.write_srt(output_prefix, &segments)?;

        debug!("Whisper transcription completed successfully");
        Ok(())
    }
}

struct SegmentData {
    start_ms: u32,
    end_ms: u32,
    text: String,
}

fn collect_segments(result: &PythonResult) -> Result<Vec<SegmentData>> {
    let mut segments = Vec::new();
    for segment in &result.segments {
        let start_ms = seconds_to_millis(segment.start);
        let end_ms = seconds_to_millis(segment.end);
        if end_ms <= start_ms {
            continue;
        }
        let text = segment.text.trim().to_string();
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

fn seconds_to_millis(seconds: f64) -> u32 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    let millis = (seconds * 1000.0).round();
    if millis <= 0.0 {
        0
    } else if millis >= u32::MAX as f64 {
        u32::MAX
    } else {
        millis as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hound;
    use std::time::Instant;
    use tempfile::tempdir;

    #[test]
    fn test_whisper_config_builder() {
        let config = WhisperConfig::new("/path/to/model".to_string())
            .with_threads(4)
            .with_language("en".to_string())
            .with_gpu_device(1)
            .with_flash_attn(true);

        assert_eq!(config.model_path, "/path/to/model");
        assert_eq!(config.threads, 4);
        assert_eq!(config.language, "en");
        assert_eq!(config.gpu_device, 1);
        assert!(config.flash_attn);
    }

    /// Smoke test for faster-whisper backend (opt-in with env MPV_STT_PLUGIN_RS_E2E_FAST_WHISPER=1).
    /// Requires Python + faster-whisper installed.
    #[test]
    #[cfg(any(feature = "fast_whisper_cpu", feature = "fast_whisper_cuda"))]
    fn test_fast_whisper_smoke_optional() {
        if std::env::var("MPV_STT_PLUGIN_RS_E2E_FAST_WHISPER").is_err() {
            eprintln!(
                "skipping fast_whisper smoke test (set MPV_STT_PLUGIN_RS_E2E_FAST_WHISPER=1 to run)"
            );
            return;
        }

        // Prepare audio: use external file if provided, otherwise synthesize silence WAV.
        let dir = tempdir().expect("tempdir");
        let audio_path = if let Ok(path) = std::env::var("MPV_STT_PLUGIN_RS_E2E_AUDIO_FILE") {
            PathBuf::from(path)
        } else {
            let wav_path = dir.path().join("silence.wav");
            let audio_ms: u32 = std::env::var("MPV_STT_PLUGIN_RS_E2E_AUDIO_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3_000);
            let samples = (audio_ms as usize * 16_000) / 1000;
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::create(&wav_path, spec).expect("create wav");
            for _ in 0..samples {
                writer.write_sample(0i16).unwrap();
            }
            writer.finalize().unwrap();
            wav_path
        };

        let model =
            std::env::var("MPV_STT_PLUGIN_RS_E2E_MODEL").unwrap_or_else(|_| "tiny".to_string());
        let build_start = Instant::now();
        let mut runner = WhisperRunner::new(
            WhisperConfig::new(model)
                .with_threads(2)
                .with_language("en".to_string())
                .with_gpu_device(0)
                .with_flash_attn(false)
                .with_timeout_ms(30_000),
        );
        let build_ms = build_start.elapsed().as_millis();

        let out_prefix = dir.path().join("out");
        let infer_start = Instant::now();
        runner
            .transcribe(&audio_path, &out_prefix, 5_000)
            .expect("fast_whisper transcribe");
        let infer_ms = infer_start.elapsed().as_millis();

        println!(
            "fast_whisper smoke: audio_path={} build_ms={} infer_ms={}",
            audio_path.display(),
            build_ms,
            infer_ms
        );
    }
}
