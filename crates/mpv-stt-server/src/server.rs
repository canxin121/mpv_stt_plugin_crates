use crate::worker::WorkerPool;
use anyhow::{Context, Result};
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::Response,
    routing::post,
};
use bytes::Bytes;
use hex::FromHex;
use log::{info, warn};
use mpv_stt_crypto::{AuthToken, EncryptionKey};
use mpv_stt_plugin::SttBackend;
use mpv_stt_protocol::{JobMetrics, JobResult, TranscriptionJob};
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio_stream::{StreamExt, wrappers::UnboundedReceiverStream};

const MAX_BODY_SIZE: usize = 50 * 1024 * 1024;
const COMPRESSION_PCM: &str = "pcm";
const COMPRESSION_WAV: &str = "wav";
const COMPRESSION_OPUS: &str = "opus";
const HEADER_QUEUE_MS: &str = "x-metric-queue-ms";
const HEADER_INFER_MS: &str = "x-metric-infer-ms";
const HEADER_WORKER_MS: &str = "x-metric-worker-ms";
const HEADER_BYTES_IN: &str = "x-bytes-in";
const HEADER_BYTES_OUT: &str = "x-bytes-out";

pub struct ServerConfig {
    pub enable_encryption: bool,
    pub encryption_key: String,
    pub auth_secret: String,
    pub warmup: bool,
}

#[derive(Clone)]
struct AppState {
    worker_tx: mpsc::UnboundedSender<TranscriptionJob>,
    result_rx: Arc<Mutex<UnboundedReceiverStream<JobResult>>>,
    encryption_key: Option<EncryptionKey>,
    expected_auth_token: Option<AuthToken>,
}

pub struct HttpServer {
    handle: JoinHandle<()>,
}

impl HttpServer {
    pub async fn bind(
        bind_addr: &str,
        whisper_config: mpv_stt_plugin::LocalModelConfig,
        num_workers: usize,
        config: ServerConfig,
    ) -> Result<Self> {
        let encryption_key = if config.enable_encryption {
            Some(EncryptionKey::from_passphrase(&config.encryption_key))
        } else {
            None
        };
        let expected_auth_token = if !config.auth_secret.is_empty() {
            Some(AuthToken::from_secret(&config.auth_secret))
        } else {
            None
        };

        let worker_pool = WorkerPool::new(whisper_config.clone(), num_workers);

        if config.warmup {
            match run_warmup(whisper_config.clone()).await {
                Ok(_) => info!("Warmup inference completed"),
                Err(e) => warn!("Warmup inference failed: {}", e),
            }
        }

        let worker_tx = worker_pool.job_sender();
        let (result_stream_tx, result_stream_rx) = mpsc::unbounded_channel();
        // Own the pool and forward results to shared stream.
        tokio::spawn(async move {
            let mut pool = worker_pool;
            while let Some(res) = pool.next_result().await {
                let _ = result_stream_tx.send(res);
            }
        });

        let state = AppState {
            worker_tx,
            result_rx: Arc::new(Mutex::new(UnboundedReceiverStream::new(result_stream_rx))),
            encryption_key,
            expected_auth_token,
        };

        let app = Router::new()
            .route("/transcribe", post(handle_transcribe))
            .with_state(state);

        let addr: SocketAddr = bind_addr.parse()?;
        let listener = TcpListener::bind(&addr).await?;
        let server = axum::serve(listener, app);

        let handle = tokio::spawn(async move {
            if let Err(e) = server.await {
                eprintln!("axum server error: {}", e);
            }
        });

        info!("HTTP server listening on {}", bind_addr);
        Ok(Self { handle })
    }

    pub async fn run(self) -> Result<()> {
        self.handle.await.unwrap();
        Ok(())
    }
}

async fn run_warmup(config: mpv_stt_plugin::LocalModelConfig) -> Result<()> {
    tokio::task::spawn_blocking(move || warmup_blocking(config)).await??;
    Ok(())
}

fn warmup_blocking(config: mpv_stt_plugin::LocalModelConfig) -> Result<()> {
    use hound::{SampleFormat, WavSpec, WavWriter};
    use tempfile::NamedTempFile;

    info!("Running warmup inference to preload model...");

    let mut runner = mpv_stt_plugin::SttRunner::new(config);
    let temp = NamedTempFile::new().context("create temp WAV for warmup")?;

    let spec = WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };

    {
        let mut writer =
            WavWriter::create(temp.path(), spec).context("create warmup WAV writer")?;
        for _ in 0..16_000 {
            writer.write_sample(0i16).context("write warmup sample")?;
        }
        writer.finalize().context("finalize warmup WAV")?;
    }

    let prefix = temp.path();
    runner
        .transcribe(prefix, prefix, 1_000)
        .context("warmup transcription")?;

    let _ = std::fs::remove_file(prefix.with_extension("srt"));
    let _ = std::fs::remove_file(prefix.with_extension("txt"));

    Ok(())
}

async fn handle_transcribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if body.len() > MAX_BODY_SIZE {
        return response_with_status(StatusCode::PAYLOAD_TOO_LARGE, b"body too large");
    }

    let request_id = match headers
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
    {
        Some(id) => id,
        None => return response_with_status(StatusCode::BAD_REQUEST, b"missing x-request-id"),
    };

    let duration_ms = headers
        .get("x-duration-ms")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    if let Some(expected) = &state.expected_auth_token {
        let ok = headers
            .get("x-auth-token")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| Vec::from_hex(s).ok())
            .and_then(|v| v.try_into().ok())
            .map(AuthToken::from_bytes)
            .map(|token| &token == expected)
            .unwrap_or(false);
        if !ok {
            return response_with_status(StatusCode::UNAUTHORIZED, b"unauthorized");
        }
    }

    let encrypted = headers
        .get("x-encrypted")
        .and_then(|h| h.to_str().ok())
        .map(|s| s == "1")
        .unwrap_or(false);

    let compression = headers
        .get("x-compression")
        .and_then(|h| h.to_str().ok())
        .unwrap_or(COMPRESSION_PCM);

    let mut audio_bytes = body.to_vec();
    if encrypted {
        if let Some(key) = state.encryption_key.as_ref() {
            match key.decrypt(&audio_bytes) {
                Ok(decrypted) => audio_bytes = decrypted,
                Err(e) => {
                    return response_with_status(
                        StatusCode::BAD_REQUEST,
                        format!("decrypt failed: {}", e).as_bytes(),
                    );
                }
            }
        } else {
            return response_with_status(StatusCode::BAD_REQUEST, b"encryption not enabled");
        }
    }

    let audio_data = match compression {
        COMPRESSION_PCM | COMPRESSION_WAV => audio_bytes,
        COMPRESSION_OPUS => match decompress_opus(&audio_bytes) {
            Ok(d) => d,
            Err(e) => {
                // Backward compatibility: some clients mislabeled WAV as OPUS.
                if audio_bytes.starts_with(b"RIFF") {
                    warn!("compression=opus but payload looks like WAV; bypassing opus decode");
                    audio_bytes
                } else {
                    return response_with_status(StatusCode::BAD_REQUEST, e.to_string().as_bytes());
                }
            }
        },
        _ => return response_with_status(StatusCode::BAD_REQUEST, b"unsupported compression"),
    };

    if audio_data.is_empty() {
        return response_with_status(StatusCode::BAD_REQUEST, b"empty audio data");
    }

    if let Err(msg) = validate_wav(&audio_data) {
        return response_with_status(StatusCode::BAD_REQUEST, msg.as_bytes());
    }

    let job = TranscriptionJob {
        request_id,
        audio_data,
        duration_ms,
        enqueue_at: Instant::now(),
    };

    if state.worker_tx.send(job).is_err() {
        return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, b"failed to enqueue job");
    }

    // Wait for result
    let (srt_data, metrics) = match wait_for_result(&state, request_id).await {
        Ok(data) => data,
        Err(msg) => return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, msg.as_bytes()),
    };

    let mut resp_body = srt_data.clone();
    if encrypted {
        if let Some(key) = state.encryption_key.as_ref() {
            match key.encrypt(&resp_body) {
                Ok(enc) => resp_body = enc,
                Err(e) => {
                    return response_with_status(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        e.to_string().as_bytes(),
                    );
                }
            }
        }
    }

    let resp_body_len = resp_body.len();
    let mut response = Response::new(resp_body.into());
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    let _ = headers.insert(
        HEADER_QUEUE_MS,
        HeaderValue::from_str(&metrics.queue_wait_ms.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    let _ = headers.insert(
        HEADER_INFER_MS,
        HeaderValue::from_str(&metrics.inference_ms.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    let _ = headers.insert(
        HEADER_WORKER_MS,
        HeaderValue::from_str(&metrics.worker_total_ms.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    let _ = headers.insert(
        HEADER_BYTES_IN,
        HeaderValue::from_str(&body.len().to_string()).unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    let _ = headers.insert(
        HEADER_BYTES_OUT,
        HeaderValue::from_str(&resp_body_len.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0")),
    );

    response
}

async fn wait_for_result(
    state: &AppState,
    request_id: u64,
) -> std::result::Result<(Vec<u8>, JobMetrics), String> {
    use tokio::time::{Duration, Instant, sleep};
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if Instant::now() > deadline {
            return Err("timeout waiting result".to_string());
        }
        let mut rx = state.result_rx.lock().await;
        match rx.next().await {
            Some(JobResult::Success {
                request_id: id,
                srt_data,
                metrics,
            }) if id == request_id => return Ok((srt_data, metrics)),
            Some(JobResult::Error {
                request_id: id,
                message,
            }) if id == request_id => return Err(message),
            Some(other) => {
                // unrelated result, put back? simplest: drop
                warn!("dropping unrelated result {:?}", other);
            }
            None => {
                return Err("result channel closed".to_string());
            }
        }
        drop(rx);
        sleep(Duration::from_millis(50)).await;
    }
}

fn response_with_status(status: StatusCode, body: &[u8]) -> Response {
    let mut resp = Response::new(body.to_vec().into());
    *resp.status_mut() = status;
    resp
}

fn decompress_opus(compressed: &[u8]) -> Result<Vec<u8>> {
    use std::convert::TryInto;
    use std::ffi::CStr;
    use std::os::raw::c_int;

    use hound::WavWriter;
    use opus_static_sys as opus;
    use tempfile::NamedTempFile;

    const SAMPLE_RATE: c_int = 16_000;
    const CHANNELS: c_int = 1;
    // 120 ms @ 48k = 5760 samples; safe upper bound for 16k streams too.
    const MAX_FRAME_SIZE: usize = 5760;

    let mut err: c_int = 0;
    let decoder = unsafe { opus::opus_decoder_create(SAMPLE_RATE, CHANNELS, &mut err) };
    if decoder.is_null() || err != opus::OPUS_OK as c_int {
        let msg = unsafe {
            CStr::from_ptr(opus::opus_strerror(err))
                .to_string_lossy()
                .into_owned()
        };
        anyhow::bail!("Failed to create Opus decoder: {}", msg);
    }

    let mut samples = Vec::new();
    let mut pos = 0;

    while pos + 4 <= compressed.len() {
        let frame_len = u32::from_le_bytes(
            compressed[pos..pos + 4]
                .try_into()
                .expect("slice length validated"),
        ) as usize;
        pos += 4;

        if pos + frame_len > compressed.len() {
            unsafe { opus::opus_decoder_destroy(decoder) };
            anyhow::bail!("Invalid Opus frame length");
        }

        let frame = &compressed[pos..pos + frame_len];
        pos += frame_len;

        let mut output = vec![0i16; MAX_FRAME_SIZE];
        let decoded_samples = unsafe {
            opus::opus_decode(
                decoder,
                frame.as_ptr(),
                frame_len as opus::opus_int32,
                output.as_mut_ptr(),
                MAX_FRAME_SIZE as c_int,
                0,
            )
        };

        if decoded_samples < 0 {
            let msg = unsafe {
                CStr::from_ptr(opus::opus_strerror(decoded_samples))
                    .to_string_lossy()
                    .into_owned()
            };
            unsafe { opus::opus_decoder_destroy(decoder) };
            anyhow::bail!("Opus decode failed: {}", msg);
        }

        samples.extend_from_slice(&output[..decoded_samples as usize]);
    }

    unsafe { opus::opus_decoder_destroy(decoder) };

    let temp_file = NamedTempFile::new().context("Failed to create temp file")?;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    {
        let mut writer =
            WavWriter::create(temp_file.path(), spec).context("Failed to create WAV writer")?;

        for sample in &samples {
            writer
                .write_sample(*sample)
                .context("Failed to write WAV sample")?;
        }

        writer.finalize().context("Failed to finalize WAV")?;
    }

    let wav_data = std::fs::read(temp_file.path()).context("Failed to read WAV file")?;

    info!(
        "Opus decompression: {} frames → {} samples → {} bytes WAV",
        (compressed.len() / 1024).max(1),
        samples.len(),
        wav_data.len()
    );

    Ok(wav_data)
}

fn validate_wav(data: &[u8]) -> std::result::Result<(), String> {
    let cursor = Cursor::new(data);
    let mut reader = hound::WavReader::new(cursor).map_err(|e| format!("invalid wav: {}", e))?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != 16_000 || spec.bits_per_sample != 16 {
        return Err(format!(
            "unsupported wav format: {}ch {}Hz {}-bit",
            spec.channels, spec.sample_rate, spec.bits_per_sample
        ));
    }
    // Ensure there is at least one sample; avoid empty payloads that whisper cannot handle.
    if reader
        .samples::<i16>()
        .next()
        .transpose()
        .unwrap_or(None)
        .is_none()
    {
        return Err("wav contains no samples".to_string());
    }
    Ok(())
}
