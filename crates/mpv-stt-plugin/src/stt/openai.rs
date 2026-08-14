use super::{BackendKind, SttBackend, SttDeviceNotice};
use log::{debug, trace};
use mpv_stt_common::{MpvSttError, Result};
use mpv_stt_srt::{SrtFile, SubtitleEntry, Timestamp};
use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime};

pub type SttOpenAiConfig = crate::config::SttOpenAiConfig;

/// OpenAI-compatible backend: posts 16 kHz mono PCM WAV chunks to
/// `POST {server}/v1/audio/transcriptions` (multipart form) and turns the
/// returned `verbose_json` segments into an SRT file.
///
/// Works with any OpenAI-compatible transcription server, including the local
/// `subtitle-gateway` (SenseVoice / MLT-Nano). Unlike the custom
/// Custom `ferrum` protocol, no server-side changes are needed beyond the standard
/// `sentence_timestamp` / `verbose_json` form fields.
pub struct OpenAiBackend {
    server_url: String,
    model: String,
    language: Option<String>,
    api_key: Option<String>,
    max_retry: usize,
    cancel_generation: Arc<AtomicU64>,
    client: Client,
}

impl OpenAiBackend {
    pub fn new(config: SttOpenAiConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(|e| MpvSttError::SttFailed(format!("HTTP client build failed: {}", e)))?;

        Ok(Self {
            server_url: normalize_server_url(&config.server_addr),
            model: config.model,
            language: config.language,
            api_key: config.api_key,
            max_retry: config.max_retry,
            cancel_generation: Arc::new(AtomicU64::new(0)),
            client,
        })
    }

    fn transcribe_impl<P: AsRef<Path>>(
        &mut self,
        audio_path: P,
        output_prefix: P,
        duration_ms: u64,
    ) -> Result<()> {
        let audio_str = audio_path
            .as_ref()
            .to_str()
            .ok_or_else(|| MpvSttError::InvalidPath("Invalid audio path".to_string()))?;

        trace!(
            "Remote OpenAI STT: {} (duration: {}ms, model: {})",
            audio_str,
            duration_ms,
            self.model
        );

        let run_generation = self.cancel_generation.load(Ordering::Relaxed);

        // The audio extractor always produces 16 kHz mono 16-bit PCM WAV; the
        // OpenAI endpoint accepts it as-is (the server resamples if needed).
        let audio_data = std::fs::read(audio_path)
            .map_err(|e| MpvSttError::SttFailed(format!("Failed to read WAV bytes: {}", e)))?;
        if audio_data.is_empty() {
            return Err(MpvSttError::SttFailed("Audio data is empty".to_string()));
        }

        let request_id = self.generate_request_id();
        let json = self.send_with_retry(
            request_id,
            &audio_data,
            duration_ms,
            run_generation,
        )?;

        if self.cancel_generation.load(Ordering::Relaxed) != run_generation {
            return Err(MpvSttError::SttCancelled);
        }

        let output_path = PathBuf::from(output_prefix.as_ref()).with_extension("srt");
        let segments = parse_segments(&json)?;

        let mut srt = SrtFile::new();
        for (i, seg) in segments.iter().enumerate() {
            let text = seg.text.trim();
            if text.is_empty() {
                continue;
            }
            let start_ms = (seg.start * 1000.0).round() as u32;
            let end_ms = ((seg.end * 1000.0).round() as u32).max(start_ms.saturating_add(1));
            srt.append_entry(SubtitleEntry {
                index: (i + 1) as u32,
                start_time: Timestamp::from_milliseconds(start_ms),
                end_time: Timestamp::from_milliseconds(end_ms),
                text: text.to_string(),
            });
        }

        srt.save(&output_path)?;
        debug!(
            "Remote OpenAI STT completed: {} segments from chunk ({}ms)",
            segments.len(),
            duration_ms
        );
        Ok(())
    }

    fn generate_request_id(&self) -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    fn send_with_retry(
        &self,
        request_id: u64,
        audio: &[u8],
        duration_ms: u64,
        run_generation: u64,
    ) -> Result<Vec<u8>> {
        let mut last_error = None;

        for attempt in 0..self.max_retry {
            if self.cancel_generation.load(Ordering::Relaxed) != run_generation {
                return Err(MpvSttError::SttCancelled);
            }

            match self.send_request(request_id, audio, duration_ms, run_generation) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    last_error = Some(e);
                    if attempt + 1 < self.max_retry {
                        debug!("OpenAI request attempt {} failed, retrying...", attempt + 1);
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }

        Err(last_error.unwrap())
    }

    fn send_request(
        &self,
        request_id: u64,
        audio: &[u8],
        duration_ms: u64,
        run_generation: u64,
    ) -> Result<Vec<u8>> {
        // Build a standard OpenAI-style multipart form.
        let file_part = Part::bytes(audio.to_vec())
            .file_name("chunk.wav")
            .mime_str("audio/wav")
            .map_err(|e| MpvSttError::SttFailed(format!("MIME error: {}", e)))?;

        let mut form = Form::new()
            .part("file", file_part)
            .text("model", self.model.clone())
            .text("response_format", "verbose_json")
            .text("sentence_timestamp", "true");
        if let Some(lang) = self.language.as_ref() {
            form = form.text("language", lang.clone());
        }

        let wall_start = Instant::now();
        let mut request = self
            .client
            .post(format!("{}/v1/audio/transcriptions", self.server_url))
            .multipart(form)
            .header("x-request-id", request_id.to_string())
            .header("x-duration-ms", duration_ms.to_string());
        if let Some(key) = self.api_key.as_ref() {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .map_err(|e| MpvSttError::SttFailed(format!("HTTP send failed: {}", e)))?;

        if self.cancel_generation.load(Ordering::Relaxed) != run_generation {
            return Err(MpvSttError::SttCancelled);
        }

        let status = response.status();
        if !status.is_success() {
            let text = response
                .text()
                .unwrap_or_else(|_| "unknown error".to_string());
            return Err(MpvSttError::SttFailed(format!(
                "Server error ({}): {}",
                status, text
            )));
        }

        let data = response
            .bytes()
            .map_err(|e| MpvSttError::SttFailed(format!("HTTP body read failed: {}", e)))?
            .to_vec();

        debug!(
            "OpenAI req {} duration_ms={} wall={}ms model={} resp_bytes={}",
            request_id,
            duration_ms,
            wall_start.elapsed().as_millis() as u64,
            self.model,
            data.len()
        );

        Ok(data)
    }
}

#[derive(Debug, Deserialize)]
struct Segment {
    #[serde(default)]
    start: f64,
    #[serde(default)]
    end: f64,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct TranscriptionResponse {
    #[serde(default)]
    segments: Vec<Segment>,
}

fn parse_segments(json: &[u8]) -> Result<Vec<Segment>> {
    let resp: TranscriptionResponse = serde_json::from_slice(json).map_err(|e| {
        MpvSttError::SttFailed(format!(
            "Failed to parse OpenAI response: {} (body: {})",
            e,
            String::from_utf8_lossy(json).chars().take(200).collect::<String>()
        ))
    })?;
    Ok(resp.segments)
}

fn normalize_server_url(raw: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("http://{}", raw)
    }
}

impl SttBackend for OpenAiBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::OpenAi
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
        None
    }
}
