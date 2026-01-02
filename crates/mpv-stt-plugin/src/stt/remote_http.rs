use super::{BackendKind, SttBackend, SttDeviceNotice};
use log::{debug, trace};
use mpv_stt_common::{MpvSttError, Result};
use mpv_stt_crypto::{AuthToken, EncryptionKey};
use mpv_stt_srt::SrtFile;
use opusic_sys as opus;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use std::path::{Path, PathBuf};
use libc;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime};

pub type RemoteSttConfig = crate::config::SttRemoteHttpConfig;

const HEADER_REQUEST_ID: &str = "x-request-id";
const HEADER_DURATION_MS: &str = "x-duration-ms";
const HEADER_AUTH_TOKEN: &str = "x-auth-token";
const HEADER_COMPRESSION: &str = "x-compression";
const HEADER_ENCRYPTED: &str = "x-encrypted";
const HEADER_QUEUE_MS: &str = "x-metric-queue-ms";
const HEADER_INFER_MS: &str = "x-metric-infer-ms";
const HEADER_WORKER_MS: &str = "x-metric-worker-ms";
const HEADER_BYTES_IN: &str = "x-bytes-in";
const HEADER_BYTES_OUT: &str = "x-bytes-out";

// HTTP payloads are raw 16 kHz mono PCM WAV bytes; advertise them truthfully.
const COMPRESSION_PCM: &str = "pcm";
const COMPRESSION_OPUS: &str = "opus";

pub struct RemoteHttpBackend {
    config: RemoteSttConfig,
    server_url: String,
    cancel_generation: Arc<AtomicU64>,
    encryption_key: Option<EncryptionKey>,
    auth_token: AuthToken,
    client: Client,
}

impl RemoteHttpBackend {
    pub fn new(config: RemoteSttConfig) -> Result<Self> {
        let encryption_key = if config.enable_encryption {
            if config.encryption_key.is_empty() {
                return Err(MpvSttError::SttFailed(
                    "Encryption enabled but encryption_key is empty".to_string(),
                ));
            }
            Some(EncryptionKey::from_passphrase(&config.encryption_key))
        } else {
            None
        };

        let auth_token = if !config.auth_secret.is_empty() {
            AuthToken::from_secret(&config.auth_secret)
        } else {
            AuthToken::from_secret("")
        };

        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(|e| MpvSttError::SttFailed(format!("HTTP client build failed: {}", e)))?;

        let server_url = normalize_server_url(&config.server_addr);

        Ok(Self {
            config,
            server_url,
            cancel_generation: Arc::new(AtomicU64::new(0)),
            encryption_key,
            auth_token,
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
            "Remote HTTP STT: {} (duration: {}ms)",
            audio_str, duration_ms
        );

        let run_generation = self.cancel_generation.load(Ordering::Relaxed);

        let audio_data = self.compress_audio(&audio_path)?;
        if audio_data.is_empty() {
            return Err(MpvSttError::SttFailed("Audio data is empty".to_string()));
        }

        let request_id = self.generate_request_id();
        let srt_data =
            self.send_request_with_retry(request_id, &audio_data, duration_ms, run_generation)?;

        if self.cancel_generation.load(Ordering::Relaxed) != run_generation {
            return Err(MpvSttError::SttCancelled);
        }

        if srt_data.iter().all(|b| b.is_ascii_whitespace()) {
            debug!("Remote HTTP STT returned empty subtitles; skipping SRT parse");
            let output_path = PathBuf::from(output_prefix.as_ref()).with_extension("srt");
            SrtFile::new().save(&output_path)?;
            return Ok(());
        }

        let srt_file = SrtFile::parse_content(&String::from_utf8_lossy(&srt_data))?;
        let output_path = PathBuf::from(output_prefix.as_ref()).with_extension("srt");
        srt_file.save(&output_path)?;

        debug!("Remote HTTP STT completed successfully");
        Ok(())
    }

    fn generate_request_id(&self) -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    fn send_request_with_retry(
        &self,
        request_id: u64,
        audio: &[u8],
        duration_ms: u64,
        run_generation: u64,
    ) -> Result<Vec<u8>> {
        let mut last_error = None;

        for attempt in 0..self.config.max_retry {
            if self.cancel_generation.load(Ordering::Relaxed) != run_generation {
                return Err(MpvSttError::SttCancelled);
            }

            match self.send_request(request_id, audio, duration_ms, run_generation) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    last_error = Some(e);
                    if attempt + 1 < self.config.max_retry {
                        debug!("HTTP request attempt {} failed, retrying...", attempt + 1);
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
        let mut payload = audio.to_vec();
        let encrypted = if let Some(key) = self.encryption_key.as_ref() {
            payload = key.encrypt(&payload)?;
            true
        } else {
            false
        };
        let payload_len = payload.len();

        let mut headers = HeaderMap::new();
        headers.insert(
            HEADER_REQUEST_ID,
            HeaderValue::from_str(&request_id.to_string())
                .map_err(|e| MpvSttError::SttFailed(format!("Header error: {}", e)))?,
        );
        headers.insert(
            HEADER_DURATION_MS,
            HeaderValue::from_str(&duration_ms.to_string())
                .map_err(|e| MpvSttError::SttFailed(format!("Header error: {}", e)))?,
        );
        headers.insert(
            HEADER_AUTH_TOKEN,
            HeaderValue::from_str(&hex::encode(self.auth_token.as_bytes()))
                .map_err(|e| MpvSttError::SttFailed(format!("Header error: {}", e)))?,
        );
        let compression = if self.config.use_opus {
            COMPRESSION_OPUS
        } else {
            COMPRESSION_PCM
        };
        headers.insert(
            HEADER_COMPRESSION,
            HeaderValue::from_static(compression),
        );
        if encrypted {
            headers.insert(HEADER_ENCRYPTED, HeaderValue::from_static("1"));
        }

        let wall_start = Instant::now();
        let response = self
            .client
            .post(format!("{}/transcribe", self.server_url))
            .headers(headers)
            .body(payload)
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

        let response_headers = response.headers().clone();
        let mut data = response
            .bytes()
            .map_err(|e| MpvSttError::SttFailed(format!("HTTP body read failed: {}", e)))?
            .to_vec();
        let raw_resp_len = data.len();

        if encrypted {
            if let Some(key) = self.encryption_key.as_ref() {
                data = key.decrypt(&data)?;
            }
        }

        let wall_ms = wall_start.elapsed().as_millis() as u64;
        let server_queue_ms = parse_u64_header(&response_headers, HEADER_QUEUE_MS);
        let server_infer_ms = parse_u64_header(&response_headers, HEADER_INFER_MS);
        let server_worker_ms = parse_u64_header(&response_headers, HEADER_WORKER_MS);
        let server_bytes_in = parse_u64_header(&response_headers, HEADER_BYTES_IN);
        let server_bytes_out = parse_u64_header(&response_headers, HEADER_BYTES_OUT);
        let server_total_ms = server_queue_ms.saturating_add(server_worker_ms);
        let network_ms = wall_ms.saturating_sub(server_total_ms);
        let server_non_infer_ms = server_worker_ms.saturating_sub(server_infer_ms);

        debug!(
            "Remote HTTP req {} duration_ms={} wall={}ms net≈{}ms srv_queue={}ms srv_worker={}ms \
             srv_infer={}ms srv_non_infer={}ms bytes_out={}B bytes_in={}B srv_bytes_out={}B resp_raw={}B",
            request_id,
            duration_ms,
            wall_ms,
            network_ms,
            server_queue_ms,
            server_worker_ms,
            server_infer_ms,
            server_non_infer_ms,
            payload_len,
            server_bytes_in,
            server_bytes_out,
            raw_resp_len
        );

        Ok(data)
    }

    fn compress_audio<P: AsRef<Path>>(&self, audio_path: P) -> Result<Vec<u8>> {
        use hound::WavReader;

        let path_ref = audio_path.as_ref();
        let mut reader = WavReader::open(path_ref)
            .map_err(|e| MpvSttError::SttFailed(format!("Failed to read WAV: {}", e)))?;

        let spec = reader.spec();
        if spec.channels != 1 || spec.sample_rate != 16000 || spec.bits_per_sample != 16 {
            return Err(MpvSttError::SttFailed(format!(
                "Unsupported WAV format: {}ch {}Hz {}-bit",
                spec.channels, spec.sample_rate, spec.bits_per_sample
            )));
        }

        if !self.config.use_opus {
            let bytes = std::fs::read(path_ref)
                .map_err(|e| MpvSttError::SttFailed(format!("Failed to read WAV bytes: {}", e)))?;
            return Ok(bytes);
        }

        // Encode to Opus (mono, 16 kHz, 20 ms frames; framing: [u32_le_len][packet]...)
        let mut encoder = SimpleOpusEncoder::new()
            .map_err(|e| MpvSttError::SttFailed(format!("Opus encoder init failed: {e}")))?;

        let frame_size = SimpleOpusEncoder::FRAME_SIZE as usize; // 20 ms @ 16 kHz
        let mut pcm: Vec<i16> = reader
            .samples::<i16>()
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| MpvSttError::SttFailed(format!("Read WAV samples failed: {}", e)))?;

        if pcm.is_empty() {
            return Err(MpvSttError::SttFailed("Audio data is empty".to_string()));
        }

        // Pad last frame with zeros if not aligned.
        let rem = pcm.len() % frame_size;
        if rem != 0 {
            pcm.extend(std::iter::repeat(0).take(frame_size - rem));
        }

        let mut encoded = Vec::with_capacity(pcm.len() / 2);
        let mut out_buf = vec![0u8; 4000]; // generous per-frame buffer

        for chunk in pcm.chunks(frame_size) {
            let len = encoder
                .encode(chunk, &mut out_buf)
                .map_err(|e| MpvSttError::SttFailed(format!("Opus encode failed: {e}")))?;
            encoded.extend_from_slice(&(len as u32).to_le_bytes());
            encoded.extend_from_slice(&out_buf[..len]);
        }

        Ok(encoded)
    }
}

// Minimal safe wrapper around opusic-sys encoder.
struct SimpleOpusEncoder {
    enc: *mut opus::OpusEncoder,
}

impl SimpleOpusEncoder {
    const SAMPLE_RATE: i32 = 16_000;
    const CHANNELS: i32 = 1;
    // 20 ms @ 16 kHz
    const FRAME_SIZE: i32 = 320;

    fn new() -> std::result::Result<Self, String> {
        let mut err: libc::c_int = 0;
        let enc = unsafe {
            opus::opus_encoder_create(
                Self::SAMPLE_RATE,
                Self::CHANNELS,
                opus::OPUS_APPLICATION_AUDIO,
                &mut err,
            )
        };
        if enc.is_null() || err != opus::OPUS_OK {
            return Err(format!("opus_encoder_create failed: {}", opus_error(err)));
        }
        Ok(Self { enc })
    }

    fn encode(&mut self, pcm: &[i16], out: &mut [u8]) -> std::result::Result<usize, String> {
        if pcm.len() != Self::FRAME_SIZE as usize {
            return Err(format!(
                "invalid frame samples: expected {}, got {}",
                Self::FRAME_SIZE,
                pcm.len()
            ));
        }
        let ret = unsafe {
            opus::opus_encode(
                self.enc,
                pcm.as_ptr(),
                Self::FRAME_SIZE,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };
        if ret < 0 {
            return Err(format!("opus_encode failed: {}", opus_error(ret)));
        }
        Ok(ret as usize)
    }
}

impl Drop for SimpleOpusEncoder {
    fn drop(&mut self) {
        unsafe { opus::opus_encoder_destroy(self.enc) };
    }
}

fn opus_error(code: libc::c_int) -> String {
    unsafe {
        let cstr = opus::opus_strerror(code);
        if cstr.is_null() {
            format!("Opus error {}", code)
        } else {
            std::ffi::CStr::from_ptr(cstr).to_string_lossy().into_owned()
        }
    }
}

fn parse_u64_header(headers: &HeaderMap, name: &str) -> u64 {
    headers
        .get(name)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

fn normalize_server_url(raw: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("http://{}", raw)
    }
}

impl SttBackend for RemoteHttpBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::RemoteHttp
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
