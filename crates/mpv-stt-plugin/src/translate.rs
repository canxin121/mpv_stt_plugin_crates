use futures::stream::StreamExt;
use log::{debug, trace, warn};
use mpv_stt_common::{MpvSttError, Result};
use mpv_stt_srt::SrtFile;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::Duration;

// Builtin Google Translate
use translators::{GoogleTranslator, Translator as GoogleTranslatorTrait};

const MAX_TRANSLATE_RETRIES: usize = 2;
const RETRY_BASE_DELAY_MS: u64 = 250;

#[derive(Clone)]
pub struct TranslatorConfig {
    pub from_lang: String,
    pub to_lang: String,
    pub timeout_ms: u64,
    pub concurrency: usize,
}

impl Default for TranslatorConfig {
    fn default() -> Self {
        Self {
            from_lang: "auto".to_string(),
            to_lang: "en".to_string(),
            timeout_ms: 30_000,
            concurrency: 4,
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
}

pub struct Translator {
    config: TranslatorConfig,
}

impl Translator {
    pub fn new(config: TranslatorConfig) -> Self {
        Self { config }
    }

    /// Translate a single text string using Google Translate
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

        self.translate_builtin(text)
    }

    fn translate_builtin(&self, text: &str) -> Result<String> {
        // Use translators crate for Google Translate
        let translator = build_google_translator(&self.config);

        // Convert language codes (translators uses different format)
        let from_lang = normalize_lang_code(&self.config.from_lang, true);
        let to_lang = normalize_lang_code(&self.config.to_lang, false);

        // Use sync version (we're already in async context via worker thread)
        match translator.translate_sync(text, &from_lang, &to_lang) {
            Ok(result) => Ok(result),
            Err(e) => Err(MpvSttError::TranslationFailed(format!(
                "Builtin translation failed: {}",
                e
            ))),
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

            // Process using builtin Google Translate
            Self::process_builtin(
                &tasks,
                &result_sender,
                &config,
                &shutdown_flag,
                &generation,
                current_generation,
                runtime,
            );

            debug!("Completed batch of {} translations", task_count);
        }
    }

    /// Process translation tasks using builtin translator
    fn process_builtin(
        tasks: &[TranslationTask],
        result_sender: &Sender<TranslationResult>,
        config: &Arc<TranslatorConfig>,
        shutdown_flag: &Arc<std::sync::atomic::AtomicBool>,
        generation: &Arc<AtomicU64>,
        task_generation: u64,
        runtime: &tokio::runtime::Runtime,
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

        runtime.block_on(async move {
            let stream = futures::stream::iter(active_tasks).map(|task| {
                let config_clone = Arc::clone(&config);
                let shutdown_clone = Arc::clone(&shutdown_flag);
                let generation_clone = Arc::clone(&generation);
                Self::translate_single_task_async(
                    task,
                    config_clone,
                    shutdown_clone,
                    generation_clone,
                    task_generation,
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
    ) -> Option<TranslationResult> {
        let translator = build_google_translator(&config);
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

            match translator
                .translate_async(&task.text, &from_lang, &to_lang)
                .await
            {
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
                        "Builtin translation returned empty for task at {}ms (attempt {})",
                        task.start_ms,
                        attempt + 1
                    );
                }
                Err(e) => {
                    warn!(
                        "Builtin translation failed for task at {}ms (attempt {}): {}",
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

fn build_google_translator(config: &TranslatorConfig) -> GoogleTranslator {
    let mut translator = GoogleTranslator::default();
    let timeout_secs = ((config.timeout_ms + 999) / 1000).max(1) as usize;
    translator.timeout = timeout_secs;
    translator
}

fn normalize_lang_code(code: &str, allow_auto: bool) -> String {
    match code {
        "auto" if allow_auto => String::new(),
        "zh" => "zh-CN".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translator_config() {
        let config =
            TranslatorConfig::new("ja".to_string(), "zh".to_string()).with_timeout_ms(5000);

        assert_eq!(config.from_lang, "ja");
        assert_eq!(config.to_lang, "zh");
        assert_eq!(config.timeout_ms, 5000);
    }
}
