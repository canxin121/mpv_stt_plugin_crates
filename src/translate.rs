use crate::config::TranslateBackendKind;
use futures::stream::StreamExt;
use log::{debug, trace, warn};
use crate::common::{MpvSttError, Result};
use crate::srt::SrtFile;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::Duration;

const MAX_TRANSLATE_RETRIES: usize = 2;
const RETRY_BASE_DELAY_MS: u64 = 250;

#[derive(Clone)]
pub struct TranslatorConfig {
    pub from_lang: String,
    pub to_lang: String,
    pub timeout_ms: u64,
    pub concurrency: usize,
    /// Which translation protocol is active (deepl | libretranslate).
    pub backend: TranslateBackendKind,
    /// DeepL-compatible translation service base URL.
    pub server_addr: String,
    /// Optional API key, sent as `Authorization: DeepL-Auth-Key {key}`.
    pub api_key: String,
    /// LibreTranslate base URL (only used when `backend == LibreTranslate`).
    pub libretranslate_server_addr: String,
    /// Optional API key, sent in the request body as `api_key` (only used for
    /// LibreTranslate).
    pub libretranslate_api_key: String,
}

impl Default for TranslatorConfig {
    fn default() -> Self {
        Self {
            from_lang: "auto".to_string(),
            to_lang: "en".to_string(),
            timeout_ms: 30_000,
            concurrency: 4,
            backend: TranslateBackendKind::default(),
            server_addr: "http://127.0.0.1:8000".to_string(),
            api_key: String::new(),
            libretranslate_server_addr: "http://127.0.0.1:8000".to_string(),
            libretranslate_api_key: String::new(),
        }
    }
}

impl TranslatorConfig {
    pub fn new(from_lang: String, to_lang: String) -> Self {
        Self {
            from_lang,
            to_lang,
            ..Default::default()
        }
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    pub fn with_backend(mut self, backend: TranslateBackendKind) -> Self {
        self.backend = backend;
        self
    }

    pub fn with_server_addr(mut self, server_addr: String) -> Self {
        self.server_addr = server_addr;
        self
    }

    pub fn with_api_key(mut self, api_key: String) -> Self {
        self.api_key = api_key;
        self
    }

    pub fn with_libretranslate_server_addr(mut self, server_addr: String) -> Self {
        self.libretranslate_server_addr = server_addr;
        self
    }

    pub fn with_libretranslate_api_key(mut self, api_key: String) -> Self {
        self.libretranslate_api_key = api_key;
        self
    }
}

pub struct Translator {
    config: TranslatorConfig,
    client: reqwest::blocking::Client,
}

impl Translator {
    pub fn new(config: TranslatorConfig) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .expect("failed to build translation HTTP client");
        Self { config, client }
    }

    /// Translate a single text string via the remote DeepL-compatible API
    pub fn translate(&self, text: &str) -> Result<String> {
        if text.is_empty() {
            return Ok(String::new());
        }

        trace!(
            "Translating text ({} -> {}): {}",
            self.config.from_lang,
            self.config.to_lang,
            text.chars().take(50).collect::<String>()
        );

        self.translate_remote(text)
    }

    fn translate_remote(&self, text: &str) -> Result<String> {
        let from_lang = normalize_lang_code(&self.config.from_lang, true);
        let to_lang = normalize_lang_code(&self.config.to_lang, false);

        let mut attempt = 0usize;
        let mut delay_ms = RETRY_BASE_DELAY_MS;
        let mut last_error: Option<MpvSttError> = None;

        loop {
            let response = match self.config.backend {
                TranslateBackendKind::DeepL => self
                    .client
                    .post(deepl_url(&self.config))
                    .headers(deepl_headers(&self.config))
                    .json(&deepl_body(&from_lang, &to_lang, text))
                    .send(),
                TranslateBackendKind::LibreTranslate => self
                    .client
                    .post(libre_url(&self.config))
                    .json(&libre_body(&from_lang, &to_lang, text, &self.config.libretranslate_api_key))
                    .send(),
            };

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    let handled = match self.config.backend {
                        TranslateBackendKind::DeepL => {
                            deepl_handle_response(status, &body, text)
                        }
                        TranslateBackendKind::LibreTranslate => {
                            libre_handle_response(status, &body, text)
                        }
                    };
                    match handled {
                        Ok(translated) if !translated.trim().is_empty() => {
                            return Ok(translated);
                        }
                        Ok(_) => warn!(
                            "Translation returned empty for '{}' (attempt {})",
                            text.chars().take(40).collect::<String>(),
                            attempt + 1
                        ),
                        Err(e) => {
                            warn!(
                                "Translation failed for '{}' (attempt {}): {}",
                                text.chars().take(40).collect::<String>(),
                                attempt + 1,
                                e
                            );
                            last_error = Some(e);
                        }
                    }
                }
                Err(e) => {
                    let err = MpvSttError::TranslationFailed(format!(
                        "Translation request failed: {}",
                        e
                    ));
                    warn!(
                        "Translation request failed for '{}' (attempt {}): {}",
                        text.chars().take(40).collect::<String>(),
                        attempt + 1,
                        e
                    );
                    last_error = Some(err);
                }
            }

            attempt += 1;
            if attempt > MAX_TRANSLATE_RETRIES {
                return Err(last_error.unwrap_or_else(|| {
                    MpvSttError::TranslationFailed("translation returned empty".to_string())
                }));
            }

            thread::sleep(Duration::from_millis(delay_ms));
            delay_ms = (delay_ms * 2).min(2_000);
        }
    }

    /// Translate an SRT file and create a bilingual version
    pub fn translate_srt_file<P: AsRef<Path>>(&self, input_path: P, output_path: P) -> Result<()> {
        debug!("Translating SRT file with {} entries", {
            let temp_srt = SrtFile::parse(&input_path)?;
            temp_srt.entries.len()
        });
        let mut srt = SrtFile::parse(&input_path)?;
        let mut translations = Vec::new();

        for entry in &srt.entries {
            match self.translate(&entry.text) {
                Ok(translated) if !translated.is_empty() => {
                    translations.push(translated);
                }
                Ok(_) => {
                    translations.push(String::new());
                }
                Err(e) => {
                    warn!("Translation warning: {}", e);
                    translations.push(String::new());
                }
            }
        }

        srt.merge_bilingual(&translations);
        srt.save(output_path)?;
        debug!("SRT translation completed");
        Ok(())
    }

    /// Batch translate multiple texts
    pub fn translate_batch(&self, texts: &[String]) -> Vec<Result<String>> {
        texts.iter().map(|text| self.translate(text)).collect()
    }
}

/// Translation task for async processing
#[derive(Debug, Clone)]
pub struct TranslationTask {
    pub start_ms: u32,
    pub text: String,
}

/// Result from async translation
#[derive(Debug, Clone)]
pub struct TranslationResult {
    pub start_ms: u32,
    pub original: String,
    pub translated: String,
}

#[derive(Debug, Clone)]
struct QueuedTask {
    generation: u64,
    task: TranslationTask,
}

/// Async translation queue that processes translations in background
pub struct AsyncTranslationQueue {
    task_sender: Sender<Option<QueuedTask>>,
    result_receiver: Receiver<TranslationResult>,
    worker_handle: Option<thread::JoinHandle<()>>,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
    generation: Arc<AtomicU64>,
}

impl AsyncTranslationQueue {
    pub fn new(config: TranslatorConfig) -> Self {
        let (task_sender, task_receiver) = channel::<Option<QueuedTask>>();
        let (result_sender, result_receiver) = channel::<TranslationResult>();

        let config = Arc::new(config);
        let shutdown_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(0));
        let shutdown_flag_clone = shutdown_flag.clone();
        let generation_clone = generation.clone();
        let worker_handle = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .enable_io()
                .build()
                .expect("failed to build tokio runtime for translator");
            Self::worker_thread(
                task_receiver,
                result_sender,
                config,
                shutdown_flag_clone,
                generation_clone,
                &runtime,
            );
        });

        Self {
            task_sender,
            result_receiver,
            worker_handle: Some(worker_handle),
            shutdown_flag,
            generation,
        }
    }

    /// Submit a translation task to the queue
    pub fn submit(&self, task: TranslationTask) {
        let generation = self.generation.load(Ordering::Relaxed);
        let _ = self.task_sender.send(Some(QueuedTask { generation, task }));
    }

    /// Try to get completed translation results (non-blocking)
    pub fn try_recv_results(&self) -> Vec<TranslationResult> {
        let mut results = Vec::new();
        while let Ok(result) = self.result_receiver.try_recv() {
            results.push(result);
        }
        results
    }

    /// Cancel any in-flight translation tasks without tearing down the worker.
    pub fn cancel_inflight(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Worker thread that processes translation tasks in batches
    fn worker_thread(
        task_receiver: Receiver<Option<QueuedTask>>,
        result_sender: Sender<TranslationResult>,
        config: Arc<TranslatorConfig>,
        shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
        generation: Arc<AtomicU64>,
        runtime: &tokio::runtime::Runtime,
    ) {
        loop {
            // Check shutdown flag
            if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
                debug!("Translation worker thread shutting down due to shutdown flag");
                return;
            }

            // Wait for first task (blocking with timeout to allow periodic shutdown checks)
            let first_task = match task_receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(Some(task)) => task,
                Ok(None) => {
                    debug!("Translation worker thread exiting (received shutdown signal)");
                    return;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Timeout, check shutdown flag again
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    debug!("Translation worker thread exiting (channel disconnected)");
                    return;
                }
            };

            let current_generation = generation.load(Ordering::Relaxed);

            // Collect all pending tasks from queue (non-blocking)
            let mut tasks = Vec::new();
            if first_task.generation == current_generation {
                tasks.push(first_task.task);
            }
            while let Ok(Some(task)) = task_receiver.try_recv() {
                if task.generation == current_generation {
                    tasks.push(task.task);
                }
            }

            if tasks.is_empty() {
                continue;
            }

            let task_count = tasks.len();
            debug!("Processing {} translation tasks", task_count);

            // Build one shared async client per batch (connection pool reused
            // across the concurrent requests).
            let client = reqwest::Client::builder()
                .timeout(Duration::from_millis(config.timeout_ms))
                .build();
            let client = match client {
                Ok(client) => client,
                Err(e) => {
                    warn!("Failed to build translation HTTP client: {}", e);
                    continue;
                }
            };

            Self::process_remote(
                &tasks,
                &result_sender,
                &config,
                &shutdown_flag,
                &generation,
                current_generation,
                runtime,
                &client,
            );

            debug!("Completed batch of {} translations", task_count);
        }
    }

    /// Process translation tasks using the remote DeepL-compatible API
    fn process_remote(
        tasks: &[TranslationTask],
        result_sender: &Sender<TranslationResult>,
        config: &Arc<TranslatorConfig>,
        shutdown_flag: &Arc<std::sync::atomic::AtomicBool>,
        generation: &Arc<AtomicU64>,
        task_generation: u64,
        runtime: &tokio::runtime::Runtime,
        client: &reqwest::Client,
    ) {
        if tasks.is_empty() {
            return;
        }

        debug!(
            "Translating {} active tasks using single-thread tokio runtime",
            tasks.len()
        );

        let active_tasks: Vec<TranslationTask> = tasks.to_vec();
        let config = Arc::clone(config);
        let shutdown_flag = Arc::clone(shutdown_flag);
        let generation = Arc::clone(generation);
        let sender = result_sender.clone();
        let concurrency = config.concurrency.max(1);
        let client = Arc::new(client.clone());

        runtime.block_on(async move {
            let stream = futures::stream::iter(active_tasks).map(|task| {
                let config_clone = Arc::clone(&config);
                let shutdown_clone = Arc::clone(&shutdown_flag);
                let generation_clone = Arc::clone(&generation);
                let client_clone = Arc::clone(&client);
                Self::translate_single_task_async(
                    task,
                    config_clone,
                    shutdown_clone,
                    generation_clone,
                    task_generation,
                    client_clone,
                )
            });

            let mut futures = stream.buffer_unordered(concurrency);

            while let Some(result) = futures.next().await {
                if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                if generation.load(Ordering::Relaxed) != task_generation {
                    break;
                }
                if let Some(result) = result {
                    if sender.send(result).is_err() {
                        debug!("Main thread dropped receiver, exiting");
                        break;
                    }
                }
            }
        });
    }

    /// Translate a single task with retry logic
    async fn translate_single_task_async(
        task: TranslationTask,
        config: Arc<TranslatorConfig>,
        shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
        generation: Arc<AtomicU64>,
        task_generation: u64,
        client: Arc<reqwest::Client>,
    ) -> Option<TranslationResult> {
        let from_lang = normalize_lang_code(&config.from_lang, true);
        let to_lang = normalize_lang_code(&config.to_lang, false);

        let mut attempt = 0usize;
        let mut delay_ms = RETRY_BASE_DELAY_MS;

        loop {
            // Check shutdown flag
            if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
                return None;
            }
            if generation.load(Ordering::Relaxed) != task_generation {
                return None;
            }

            let translated = match config.backend {
                TranslateBackendKind::DeepL => {
                    deepl_translate_async(&client, &config, &from_lang, &to_lang, &task.text).await
                }
                TranslateBackendKind::LibreTranslate => {
                    libre_translate_async(&client, &config, &from_lang, &to_lang, &task.text).await
                }
            };
            match translated {
                Ok(translated) if !translated.trim().is_empty() => {
                    if generation.load(Ordering::Relaxed) != task_generation {
                        return None;
                    }
                    return Some(TranslationResult {
                        start_ms: task.start_ms,
                        original: task.text.clone(),
                        translated,
                    });
                }
                Ok(_) => {
                    warn!(
                        "Translation returned empty for task at {}ms (attempt {})",
                        task.start_ms,
                        attempt + 1
                    );
                }
                Err(e) => {
                    warn!(
                        "Translation failed for task at {}ms (attempt {}): {}",
                        task.start_ms,
                        attempt + 1,
                        e
                    );
                }
            }

            attempt += 1;
            if attempt > MAX_TRANSLATE_RETRIES {
                return None;
            }

            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            delay_ms = (delay_ms * 2).min(2_000);
        }
    }

    /// Shutdown the worker thread gracefully
    pub fn shutdown(&mut self) {
        debug!("Shutting down async translation queue");
        // Send shutdown signal
        let _ = self.task_sender.send(None);

        // Wait for worker thread to finish (with timeout)
        if let Some(handle) = self.worker_handle.take() {
            // Try to join with a reasonable timeout
            match handle.join() {
                Ok(_) => debug!("Translation worker thread shut down successfully"),
                Err(_) => warn!("Translation worker thread panicked during shutdown"),
            }
        }
    }

    /// Force immediate shutdown by disconnecting channels
    pub fn force_shutdown(&mut self) {
        debug!("Force shutting down async translation queue");

        // Set shutdown flag to kill any running crow processes
        self.shutdown_flag
            .store(true, std::sync::atomic::Ordering::Relaxed);

        // Send shutdown signal
        let _ = self.task_sender.send(None);

        // Wait briefly for worker thread to exit
        if let Some(handle) = self.worker_handle.take() {
            // Give it a short time to clean up
            let _result = std::thread::spawn(move || handle.join());

            // Wait max 500ms for graceful shutdown
            std::thread::sleep(Duration::from_millis(500));

            // If still running, just drop it (thread will be detached)
            debug!("Translation worker shutdown completed");
        }
    }
}

impl Drop for AsyncTranslationQueue {
    fn drop(&mut self) {
        // Force immediate shutdown on drop
        if self.worker_handle.is_some() {
            debug!("AsyncTranslationQueue dropped, forcing shutdown");
            let _ = self.task_sender.send(None);
            // Don't wait in Drop to avoid blocking
        }
    }
}

/// DeepL-compatible API helpers (shared by the blocking `Translator` and the
/// async queue path). Wire format: POST {server}/v1/translate with JSON body
/// `{"text": [..], "target_lang": "ZH", "source_lang": "EN"}` and an optional
/// `Authorization: DeepL-Auth-Key {key}` header. Response
/// `{"translations": [{"detected_source_language", "text"}]}`.

fn deepl_url(config: &TranslatorConfig) -> String {
    format!(
        "{}/v1/translate",
        config.server_addr.trim_end_matches('/')
    )
}

fn deepl_headers(config: &TranslatorConfig) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if !config.api_key.is_empty() {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!(
            "DeepL-Auth-Key {}",
            config.api_key
        )) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
    }
    headers
}

fn deepl_body(from_lang: &str, to_lang: &str, text: &str) -> serde_json::Value {
    let mut body = serde_json::json!({
        "text": [text],
        "target_lang": to_lang.to_uppercase(),
    });
    if !from_lang.is_empty() && from_lang != "auto" {
        body["source_lang"] = serde_json::Value::String(from_lang.to_uppercase());
    }
    body
}

fn deepl_handle_response(
    status: reqwest::StatusCode,
    body: &str,
    text: &str,
) -> Result<String> {
    if status.is_success() {
        return parse_deepl_response(body, text);
    }
    // DeepL error bodies are `{"message": "..."}`.
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or_else(|| format!("HTTP {}", status));
    Err(MpvSttError::TranslationFailed(format!(
        "Translation upstream error ({}): {}",
        status, message
    )))
}

fn parse_deepl_response(body: &str, text: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| MpvSttError::TranslationFailed(format!("Invalid translation response: {}", e)))?;
    value
        .get("translations")
        .and_then(|t| t.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("text"))
        .and_then(|t| t.as_str())
        .map(String::from)
        .ok_or_else(|| {
            MpvSttError::TranslationFailed(format!(
                "No translation text in response for '{}'",
                text.chars().take(50).collect::<String>()
            ))
        })
}

/// Async single-shot DeepL request (used by the async queue worker).
async fn deepl_translate_async(
    client: &reqwest::Client,
    config: &TranslatorConfig,
    from_lang: &str,
    to_lang: &str,
    text: &str,
) -> Result<String> {
    let response = client
        .post(deepl_url(config))
        .headers(deepl_headers(config))
        .json(&deepl_body(from_lang, to_lang, text))
        .send()
        .await
        .map_err(|e| MpvSttError::TranslationFailed(format!("Translation request failed: {}", e)))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_default();
    deepl_handle_response(status, &body, text)
}

/// LibreTranslate-compatible API helpers. Wire format: POST {server}/translate
/// with JSON body `{"q": "text", "source": "auto", "target": "zh",
/// "format": "text", "api_key": "..."}` (key optional, in body — LibreTranslate
/// does NOT use an Authorization header). Response: single q →
/// `{"translatedText": "..."}`; array q → `{"translations": [...]}`.

fn libre_url(config: &TranslatorConfig) -> String {
    format!(
        "{}/translate",
        config.libretranslate_server_addr.trim_end_matches('/')
    )
}

fn libre_body(from_lang: &str, to_lang: &str, text: &str, api_key: &str) -> serde_json::Value {
    let mut body = serde_json::json!({
        "q": text,
        "target": to_lang.to_lowercase(),
        "format": "text",
    });
    // LibreTranslate treats a missing/empty source as "auto" (its default), so
    // the pre-normalized "" (auto) simply omits `source` — same shape as
    // deepl_body, only lowercase.
    if !from_lang.is_empty() && from_lang != "auto" {
        body["source"] = serde_json::Value::String(from_lang.to_lowercase());
    }
    if !api_key.is_empty() {
        body["api_key"] = serde_json::Value::String(api_key.to_string());
    }
    body
}

fn libre_handle_response(
    status: reqwest::StatusCode,
    body: &str,
    text: &str,
) -> Result<String> {
    if status.is_success() {
        return parse_libre_response(body, text);
    }
    // LibreTranslate error bodies are `{"error": "..."}` (NOT {"message"}).
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| format!("HTTP {}", status));
    Err(MpvSttError::TranslationFailed(format!(
        "Translation upstream error ({}): {}",
        status, message
    )))
}

fn parse_libre_response(body: &str, text: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| MpvSttError::TranslationFailed(format!("Invalid translation response: {}", e)))?;
    // Single-q form first; array form handled defensively (client sends single q).
    if let Some(t) = value.get("translatedText").and_then(|t| t.as_str()) {
        return Ok(t.to_string());
    }
    value
        .get("translations")
        .and_then(|t| t.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("translatedText"))
        .and_then(|t| t.as_str())
        .map(String::from)
        .ok_or_else(|| {
            MpvSttError::TranslationFailed(format!(
                "No translation text in response for '{}'",
                text.chars().take(50).collect::<String>()
            ))
        })
}

/// Async single-shot LibreTranslate request (used by the async queue worker).
async fn libre_translate_async(
    client: &reqwest::Client,
    config: &TranslatorConfig,
    from_lang: &str,
    to_lang: &str,
    text: &str,
) -> Result<String> {
    let response = client
        .post(libre_url(config))
        .json(&libre_body(from_lang, to_lang, text, &config.libretranslate_api_key))
        .send()
        .await
        .map_err(|e| MpvSttError::TranslationFailed(format!("Translation request failed: {}", e)))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_default();
    libre_handle_response(status, &body, text)
}

fn normalize_lang_code(code: &str, allow_auto: bool) -> String {
    match code {
        "auto" if allow_auto => String::new(), // DeepL omits source_lang = auto
        other => other.to_string(),            // DeepL codes: zh / ja / en (no zh-CN rewrite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Spawn a minimal in-process DeepL-compatible stub server. Each request is
    /// passed (as raw header text) to `respond`, which returns (status, body).
    fn spawn_stub_deepl(
        respond: impl Fn(&str) -> (u16, String) + Send + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let mut header_end = 0usize;
                loop {
                    let n = stream.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = pos + 4;
                        break;
                    }
                    if buf.len() > 65_536 {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let (status, body) = respond(&head);
                let reason = if status == 200 { "OK" } else { "ERROR" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{}", addr)
    }

    /// Like spawn_stub_deepl but also reads the request body (Content-Length
    /// aware) so LibreTranslate tests can assert body fields (api_key /
    /// source / target). Each request is passed (head, body) to `respond`.
    fn spawn_stub_libre(
        respond: impl Fn(&str, &str) -> (u16, String) + Send + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let mut header_end = 0usize;
                loop {
                    let n = stream.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = pos + 4;
                        break;
                    }
                    if buf.len() > 65_536 {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                // Read the request body per Content-Length (present for JSON POSTs).
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        let lower = line.to_lowercase();
                        lower.strip_prefix("content-length:").map(|v| {
                            v.trim().parse::<usize>().unwrap_or(0)
                        })
                    })
                    .unwrap_or(0);
                let mut body_buf = buf[header_end..].to_vec();
                while body_buf.len() < content_length {
                    let n = stream.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body_buf.extend_from_slice(&tmp[..n]);
                }
                let body = String::from_utf8_lossy(&body_buf[..content_length.min(body_buf.len())])
                    .to_string();
                let (status, response_body) = respond(&head, &body);
                let reason = if status == 200 { "OK" } else { "ERROR" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{}", addr)
    }

    #[test]
    fn test_translator_config() {
        let config = TranslatorConfig::new("ja".to_string(), "zh".to_string())
            .with_timeout_ms(5000)
            .with_server_addr("http://127.0.0.1:8000".to_string())
            .with_api_key("k".to_string());

        assert_eq!(config.from_lang, "ja");
        assert_eq!(config.to_lang, "zh");
        assert_eq!(config.timeout_ms, 5000);
        assert_eq!(config.server_addr, "http://127.0.0.1:8000");
        assert_eq!(config.api_key, "k");
    }

    #[test]
    fn test_translate_remote_with_stub_server() {
        let server = spawn_stub_deepl(|head| {
            assert!(head.starts_with("POST /v1/translate HTTP/1.1"), "got: {}", head);
            assert!(
                head.to_lowercase().contains("authorization: deepl-auth-key testkey"),
                "missing auth header, got: {}",
                head
            );
            (
                200,
                r#"{"translations":[{"detected_source_language":"EN","text":"你好"}]}"#
                    .to_string(),
            )
        });
        let config = TranslatorConfig::new("en".to_string(), "zh".to_string())
            .with_server_addr(server)
            .with_api_key("testkey".to_string());
        let translator = Translator::new(config);
        let result = translator.translate("hello").unwrap();
        assert_eq!(result, "你好");
    }

    #[test]
    fn test_translate_remote_handles_upstream_error() {
        let server = spawn_stub_deepl(|_| (401, r#"{"message":"bad key"}"#.to_string()));
        let config = TranslatorConfig::new("en".to_string(), "zh".to_string())
            .with_server_addr(server);
        let translator = Translator::new(config);
        let msg = format!("{}", translator.translate("hello").unwrap_err());
        assert!(msg.contains("401"), "got: {}", msg);
        assert!(msg.contains("bad key"), "got: {}", msg);
    }

    #[test]
    fn test_translate_async_with_stub_server() {
        let server = spawn_stub_deepl(|_| {
            (
                200,
                r#"{"translations":[{"detected_source_language":"EN","text":"你好"}]}"#
                    .to_string(),
            )
        });
        let config = TranslatorConfig::new("en".to_string(), "zh".to_string())
            .with_server_addr(server);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap();
            let result =
                deepl_translate_async(&client, &config, "en", "zh", "hello").await.unwrap();
            assert_eq!(result, "你好");
        });
    }

    #[test]
    fn test_normalize_lang_code_deepl() {
        // DeepL codes pass through as-is; "auto" becomes empty (omit source).
        assert_eq!(normalize_lang_code("auto", true), "");
        assert_eq!(normalize_lang_code("zh", true), "zh");
        assert_eq!(normalize_lang_code("ja", false), "ja");
    }

    /// End-to-end check against a running subtitle-gateway. Ignored by
    /// default; run manually with the gateway on :8100 (api_key=testkey):
    ///   cargo test -p mpv-stt-plugin --lib -- --ignored translate_against_live_gateway
    #[test]
    #[ignore]
    fn translate_against_live_gateway() {
        let config = TranslatorConfig::new("en".to_string(), "zh".to_string())
            .with_server_addr("http://127.0.0.1:8100".to_string())
            .with_api_key("testkey".to_string())
            .with_timeout_ms(10_000);
        let translator = Translator::new(config);
        let result = translator.translate("hello").expect("live gateway translation failed");
        assert_eq!(result, "你好");
    }

    #[test]
    fn test_translate_remote_libretranslate_with_stub_server() {
        let server = spawn_stub_libre(|head, body| {
            assert!(
                head.starts_with("POST /translate HTTP/1.1"),
                "got: {}",
                head
            );
            assert!(
                !head.to_lowercase().contains("authorization"),
                "LibreTranslate must not send an Authorization header, got: {}",
                head
            );
            let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(parsed["api_key"], "testkey");
            assert_eq!(parsed["target"], "zh"); // lowercase, unlike DeepL's uppercase
            assert_eq!(parsed["source"], "en"); // explicit source passes through lowercase
            assert_eq!(parsed["q"], "hello");
            (200, r#"{"translatedText":"你好"}"#.to_string())
        });
        let config = TranslatorConfig::new("en".to_string(), "zh".to_string())
            .with_backend(TranslateBackendKind::LibreTranslate)
            .with_libretranslate_server_addr(server)
            .with_libretranslate_api_key("testkey".to_string());
        let translator = Translator::new(config);
        let result = translator.translate("hello").unwrap();
        assert_eq!(result, "你好");
    }

    #[test]
    fn test_translate_remote_libretranslate_handles_upstream_error() {
        let server = spawn_stub_libre(|_, _| (401, r#"{"error":"bad key"}"#.to_string()));
        let config = TranslatorConfig::new("en".to_string(), "zh".to_string())
            .with_backend(TranslateBackendKind::LibreTranslate)
            .with_libretranslate_server_addr(server);
        let translator = Translator::new(config);
        let msg = format!("{}", translator.translate("hello").unwrap_err());
        assert!(msg.contains("401"), "got: {}", msg);
        assert!(msg.contains("bad key"), "got: {}", msg);
    }

    #[test]
    fn test_libre_body_lang_semantics() {
        // Empty (auto) source omits the key; target stays lowercase.
        let auto = libre_body(&normalize_lang_code("auto", true), "zh", "hello", "");
        assert!(auto.get("source").is_none(), "auto must omit source, got: {}", auto);
        assert_eq!(auto["target"], "zh");
        assert!(auto.get("api_key").is_none());

        // Explicit source + key are included, lowercased.
        let full = libre_body("EN", "ZH", "hello", "k");
        assert_eq!(full["source"], "en");
        assert_eq!(full["target"], "zh");
        assert_eq!(full["api_key"], "k");
    }

    #[test]
    fn test_translate_async_libretranslate_with_stub_server() {
        let server = spawn_stub_libre(|head, body| {
            assert!(head.starts_with("POST /translate HTTP/1.1"), "got: {}", head);
            let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(parsed["api_key"], "k");
            // Array response form to cover the parse-array branch.
            (
                200,
                r#"{"translations":[{"detectedLanguage":{"confidence":100,"language":"en"},"translatedText":"你好"}]}"#
                    .to_string(),
            )
        });
        let config = TranslatorConfig::new("en".to_string(), "zh".to_string())
            .with_backend(TranslateBackendKind::LibreTranslate)
            .with_libretranslate_server_addr(server)
            .with_libretranslate_api_key("k".to_string());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap();
            let result =
                libre_translate_async(&client, &config, "en", "zh", "hello").await.unwrap();
            assert_eq!(result, "你好");
        });
    }

    /// End-to-end check against a running subtitle-gateway /translate gateway.
    /// Ignored by default; run manually with the gateway on :8100
    /// (api_key=testkey) with --libretranslate-upstream pointing at a stub:
    ///   cargo test -p mpv-stt-plugin --lib -- --ignored translate_libretranslate_against_live_gateway
    #[test]
    #[ignore]
    fn translate_libretranslate_against_live_gateway() {
        let config = TranslatorConfig::new("en".to_string(), "zh".to_string())
            .with_backend(TranslateBackendKind::LibreTranslate)
            .with_libretranslate_server_addr("http://127.0.0.1:8100".to_string())
            .with_libretranslate_api_key("testkey".to_string())
            .with_timeout_ms(10_000);
        let translator = Translator::new(config);
        let result = translator.translate("hello").expect("live gateway libretranslate failed");
        assert_eq!(result, "你好");
    }
}
