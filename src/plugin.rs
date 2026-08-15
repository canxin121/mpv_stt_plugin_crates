use log::{debug, error, info, trace, warn};
use mpv_client::{Event, Handle, mpv_handle};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::Instant;
use tempfile::TempDir;

#[cfg(target_os = "android")]
use std::ffi::CString;

use crate::audio::AudioExtractor;
use crate::common::MpvSttError;
use crate::config::Config;
use crate::srt::SrtFile;
use crate::stt::{SttBackend, SttDeviceNotice, SttRunner};
use crate::subtitle_manager::SubtitleManager;
use crate::translate::{AsyncTranslationQueue, TranslationTask, TranslatorConfig};

struct TempPaths {
    _dir: TempDir,
    tmp_wav: PathBuf,
    tmp_sub: PathBuf,
    tmp_cache: PathBuf,
}

impl TempPaths {
    fn new() -> crate::common::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("mpv_stt_plugin_rs_")
            .tempdir()?;

        Ok(Self {
            tmp_wav: dir.path().join("audio.wav"),
            // `tmp_sub` is a prefix; intermediate files are derived via `format!("{}_append...", tmp_sub.display())`
            // and the main subtitle file is `tmp_sub.with_extension("srt")`.
            tmp_sub: dir.path().join("subs"),
            tmp_cache: dir.path().join("cache.mkv"),
            _dir: dir,
        })
    }

    fn cleanup_intermediate_subs(&self) {
        let _ = std::fs::remove_file(format!("{}_append.srt", self.tmp_sub.display()));
        let _ = std::fs::remove_file(format!("{}_append_offset.srt", self.tmp_sub.display()));
        let _ = std::fs::remove_file(format!("{}_append_offset_bi.srt", self.tmp_sub.display()));
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.tmp_wav);
        let _ = std::fs::remove_file(self.tmp_sub.with_extension("srt"));
        self.cleanup_intermediate_subs();
        let _ = std::fs::remove_file(&self.tmp_cache);
    }
}

#[derive(Clone)]
struct CachePaths {
    subtitle_path: PathBuf,
    manifest_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct TranslationCacheEntry {
    start_ms: u32,
    original: String,
    translated: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct CacheManifest {
    chunk_size_ms: u64,
    processed_chunks: Vec<u64>,
    translations: Vec<TranslationCacheEntry>,
}

enum ProcessingMode {
    Network,
    Local {
        media_path: String,
        file_length_ms: u64,
        subtitle_path: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlCommand {
    ToggleStt,
    ToggleTranslate,
    ClearCache,
}

impl ControlCommand {
    fn from_client_message(args: &[&str]) -> Option<Self> {
        // `script-message-to` normally puts the payload command in args[0].
        // Accept args[1] as well for compatibility with older mpv/IINA builds
        // and user input.conf entries that included an extra routing token.
        args.iter().take(2).find_map(|arg| match *arg {
            "toggle-stt" => Some(Self::ToggleStt),
            "toggle-translate" => Some(Self::ToggleTranslate),
            "clear-cache" => Some(Self::ClearCache),
            _ => None,
        })
    }
}

const KEY_BINDINGS: [(&str, &str); 3] = [
    ("Ctrl+Shift+S", "toggle-stt"),
    ("Ctrl+Shift+T", "toggle-translate"),
    ("Ctrl+Shift+C", "clear-cache"),
];

fn key_binding_section(target: &str) -> String {
    KEY_BINDINGS
        .iter()
        .map(|(key, command)| format!("{key} script-message-to {target} {command}"))
        .collect::<Vec<_>>()
        .join("\n")
}

struct TranscriptionJob {
    generation: u64,
    media_path: String,
    audio_start_ms: u64,
    duration_ms: u64,
    wav_path: PathBuf,
    output_prefix: PathBuf,
}

struct TranscriptionWorkerResult {
    generation: u64,
    result: crate::common::Result<()>,
    device_notice: Option<SttDeviceNotice>,
}

struct TranscriptionWorker {
    job_sender: Sender<Option<TranscriptionJob>>,
    result_receiver: Receiver<TranscriptionWorkerResult>,
    worker_handle: Option<thread::JoinHandle<()>>,
    generation: Arc<AtomicU64>,
    audio_canceller: AudioExtractor,
    stt_cancel_generation: Arc<AtomicU64>,
}

impl TranscriptionWorker {
    fn new(audio_extractor: AudioExtractor, mut stt_runner: SttRunner) -> Self {
        let (job_sender, job_receiver) = channel::<Option<TranscriptionJob>>();
        let (result_sender, result_receiver) = channel::<TranscriptionWorkerResult>();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        let worker_audio = audio_extractor.clone();
        let stt_cancel_generation = stt_runner.cancellation_generation();

        let worker_handle = thread::Builder::new()
            .name("mpv-stt-transcription".to_string())
            .spawn(move || {
                while let Ok(Some(job)) = job_receiver.recv() {
                    if worker_generation.load(Ordering::Acquire) != job.generation {
                        continue;
                    }

                    let result = worker_audio
                        .extract_audio_segment(
                            job.media_path.as_str(),
                            job.wav_path.to_str().unwrap_or_default(),
                            job.audio_start_ms,
                            job.duration_ms,
                        )
                        .and_then(|()| {
                            if worker_generation.load(Ordering::Acquire) != job.generation {
                                return Err(MpvSttError::SttCancelled);
                            }
                            stt_runner.transcribe(
                                job.wav_path.to_str().unwrap_or_default(),
                                job.output_prefix.to_str().unwrap_or_default(),
                                job.duration_ms,
                            )
                        });
                    let device_notice = stt_runner.take_device_notice();

                    let _ = result_sender.send(TranscriptionWorkerResult {
                        generation: job.generation,
                        result,
                        device_notice,
                    });
                }
            })
            .expect("failed to spawn transcription worker");

        Self {
            job_sender,
            result_receiver,
            worker_handle: Some(worker_handle),
            generation,
            audio_canceller: audio_extractor,
            stt_cancel_generation,
        }
    }

    fn submit(
        &self,
        media_path: String,
        audio_start_ms: u64,
        duration_ms: u64,
        wav_path: PathBuf,
        output_prefix: PathBuf,
    ) -> Option<u64> {
        let generation = self.generation.load(Ordering::Acquire);
        let job = TranscriptionJob {
            generation,
            media_path,
            audio_start_ms,
            duration_ms,
            wav_path,
            output_prefix,
        };
        self.job_sender.send(Some(job)).ok().map(|()| generation)
    }

    fn try_recv(&self) -> Option<TranscriptionWorkerResult> {
        let current = self.generation.load(Ordering::Acquire);
        while let Ok(result) = self.result_receiver.try_recv() {
            if result.generation == current {
                return Some(result);
            }
            trace!(
                "Dropping stale transcription result from generation {} (current {})",
                result.generation, current
            );
        }
        None
    }

    fn cancel_inflight(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.audio_canceller.cancel_inflight();
        self.stt_cancel_generation.fetch_add(1, Ordering::AcqRel);
    }

    fn shutdown(&mut self) {
        if self.worker_handle.is_none() {
            return;
        }
        self.cancel_inflight();
        let _ = self.job_sender.send(None);
        if let Some(handle) = self.worker_handle.take() {
            if handle.join().is_err() {
                warn!("Transcription worker panicked during shutdown");
            }
        }
    }
}

impl Drop for TranscriptionWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct PendingTranscription {
    generation: u64,
    start_ms: u64,
    duration_ms: u64,
    subtitle_path: Option<PathBuf>,
}

struct PluginState {
    config: Config,
    paths: TempPaths,
    transcription_worker: TranscriptionWorker,
    pending_transcription: Option<PendingTranscription>,
    async_translation_queue: Option<AsyncTranslationQueue>,
    subtitle_manager: SubtitleManager,
    translation_cache: HashMap<u32, (String, String)>,
    processed_chunks: HashSet<u64>,
    network_cache: Option<CachePaths>,

    running: bool,
    shutting_down: bool,
    translate_enabled: bool, // Ctrl+Shift+t toggles whether new STT output gets translated
    subs_loaded: bool,
    current_pos_ms: u64,
    last_playback_pos_ms: Option<u64>,
    last_playback_instant: Option<Instant>,
    chunk_dur: u64,
    mode: Option<ProcessingMode>,
    pending_auto_start: bool, // Delayed auto-start after file loads
    file_loaded: bool,        // Track if file is ready
    transcription_complete: bool,
}

impl PluginState {
    fn new(config: Config) -> crate::common::Result<Self> {
        let chunk_dur = config.chunk.local_ms;
        let audio_extractor = AudioExtractor::default()
            .with_ffmpeg_timeout(config.timeout.ffmpeg_ms)
            .with_ffprobe_timeout(config.timeout.ffprobe_ms);

        // Initialize the STT backend chosen at runtime by [stt] backend key.
        // Both remote backends are compiled in; `from_config` matches the
        // `config.stt.backend` enum to the active one.
        let stt_runner = SttRunner::from_config(&config.stt)?;
        let transcription_worker = TranscriptionWorker::new(audio_extractor, stt_runner);
        let paths = TempPaths::new()?;

        // Initialize async translation queue (always enabled)
        let async_translation_queue = Some(AsyncTranslationQueue::new(
            Self::build_translator_config(&config),
        ));

        Ok(Self {
            chunk_dur,
            config,
            paths,
            transcription_worker,
            pending_transcription: None,
            async_translation_queue,
            subtitle_manager: SubtitleManager::new(),
            translation_cache: HashMap::new(),
            processed_chunks: HashSet::new(),
            network_cache: None,
            running: false,
            shutting_down: false,
            translate_enabled: true,
            subs_loaded: false,
            current_pos_ms: 0,
            last_playback_pos_ms: None,
            last_playback_instant: None,
            mode: None,
            pending_auto_start: false,
            file_loaded: false,
            transcription_complete: false,
        })
    }

    fn build_translator_config(config: &Config) -> TranslatorConfig {
        let default_libretranslate = crate::config::TranslateLibreTranslateConfig::default();
        let libretranslate = config
            .translate
            .libretranslate
            .as_ref()
            .unwrap_or(&default_libretranslate);
        TranslatorConfig::new(
            config.translate.from_lang.clone(),
            config.translate.to_lang.clone(),
        )
        .with_backend(config.translate.backend)
        .with_timeout_ms(config.timeout.translate_ms)
        .with_concurrency(config.translate.concurrency)
        .with_server_addr(config.translate.server_addr.clone())
        .with_api_key(config.translate.api_key.clone())
        .with_libretranslate_server_addr(libretranslate.server_addr.clone())
        .with_libretranslate_api_key(libretranslate.api_key.clone())
    }

    fn local_chunk_size(&self) -> u64 {
        self.config.chunk.local_ms.max(1)
    }

    fn network_chunk_size(&self) -> u64 {
        self.config.chunk.network_ms.max(1)
    }

    fn active_chunk_size(&self) -> u64 {
        match self.mode {
            Some(ProcessingMode::Network) => self.network_chunk_size(),
            Some(ProcessingMode::Local { .. }) => self.local_chunk_size(),
            None => self.local_chunk_size(),
        }
    }

    fn cancel_translation_inflight(&mut self) {
        if let Some(queue) = self.async_translation_queue.as_ref() {
            queue.cancel_inflight();
        }
    }

    fn enqueue_missing_translations_for_chunk(&mut self, chunk_start_ms: u64) {
        let Some(queue) = self.async_translation_queue.as_ref() else {
            return;
        };

        let chunk_end = chunk_start_ms.saturating_add(self.active_chunk_size());
        let entries = self.subtitle_manager.entries_in_range(
            chunk_start_ms as u32,
            chunk_end.min(u64::from(u32::MAX)) as u32,
        );

        if entries.is_empty() {
            return;
        }

        let mut pending_tasks = Vec::new();
        let mut already_translated = 0usize;

        for (start_ms, entry) in entries {
            let original = entry.text.trim();
            if original.is_empty() {
                continue;
            }

            if let Some((_, translated)) = self.translation_cache.get(&start_ms) {
                if !translated.trim().is_empty() {
                    self.subtitle_manager
                        .update_translation(start_ms, translated);
                    already_translated += 1;
                    continue;
                }
            }

            if SubtitleManager::text_has_translation(&entry.text) {
                already_translated += 1;
                continue;
            }

            pending_tasks.push(TranslationTask {
                start_ms,
                text: entry.text.clone(),
            });
        }

        if !pending_tasks.is_empty() {
            trace!(
                "Re-queueing {} missing translations (already translated: {})",
                pending_tasks.len(),
                already_translated
            );
            for task in pending_tasks {
                queue.submit(task);
            }
        }
    }

    fn toggle_stt(&mut self, client: &mut Handle) {
        if self.running {
            info!("Disabling STT");
            let _ = client.command(&["show-text", "STT: Off"]);
            self.stop_transcription();
        } else {
            info!("Enabling STT");
            if self.mode.is_some() {
                self.stop_transcription();
            }
            self.running = true;
            let _ = client.command(&["show-text", "STT: On"]);
            self.start_transcription(client);
        }
    }

    fn toggle_translate(&mut self, client: &mut Handle) {
        self.translate_enabled = !self.translate_enabled;
        info!(
            "Translation {}",
            if self.translate_enabled {
                "enabled"
            } else {
                "disabled"
            }
        );
        let msg = if self.translate_enabled {
            "Translate: On"
        } else {
            "Translate: Off (new subtitles stay as original)"
        };
        let _ = client.command(&["show-text", msg, "3000"]);
    }

    /// Delete the current media's on-disk subtitle/translation cache and drop
    /// the in-memory translation + processed-chunk state, so replaying the same
    /// file re-transcribes instead of reusing stale cached subtitles.
    fn clear_cache(&mut self, client: &mut Handle) {
        let mut removed = 0usize;
        if let Some(media_id) = Self::media_id_for_cache(client) {
            if let Some(paths) = self.cache_paths_for_media(&media_id) {
                for p in [&paths.subtitle_path, &paths.manifest_path] {
                    if p.exists() {
                        match std::fs::remove_file(p) {
                            Ok(()) => removed += 1,
                            Err(e) => error!("Failed to remove cache file {}: {}", p.display(), e),
                        }
                    }
                }
            }
        }
        // Drop in-memory state so a fresh playback re-transcribes from scratch.
        let chunk_entries = self.translation_cache.len();
        self.translation_cache.clear();
        self.processed_chunks.clear();

        info!(
            "Subtitle cache cleared (removed {} files, dropped {} cached translations)",
            removed, chunk_entries
        );
        let msg = format!(
            "字幕缓存已清除: 删除 {} 个文件, 内存缓存 {} 条",
            removed, chunk_entries
        );
        let _ = client.command(&["show-text", &msg, "3000"]);
    }

    fn schedule_transcription(
        &mut self,
        media_path: String,
        audio_start_ms: u64,
        duration_ms: u64,
        subtitle_path: Option<PathBuf>,
    ) -> bool {
        if self.pending_transcription.is_some() {
            return false;
        }

        let output_prefix = PathBuf::from(format!("{}_append", self.paths.tmp_sub.display()));
        let Some(generation) = self.transcription_worker.submit(
            media_path,
            audio_start_ms,
            duration_ms,
            self.paths.tmp_wav.clone(),
            output_prefix,
        ) else {
            error!("Transcription worker is unavailable");
            return false;
        };

        self.pending_transcription = Some(PendingTranscription {
            generation,
            start_ms: self.current_pos_ms,
            duration_ms,
            subtitle_path,
        });
        true
    }

    fn poll_transcription(&mut self, client: &mut Handle) {
        let Some(worker_result) = self.transcription_worker.try_recv() else {
            return;
        };
        let Some(pending) = self.pending_transcription.take() else {
            return;
        };
        if worker_result.generation != pending.generation {
            return;
        }

        match worker_result.result {
            Ok(()) => {
                if self.check_seek(client) {
                    debug!("Seek detected after transcription; dropping stale result");
                    self.paths.cleanup_intermediate_subs();
                    return;
                }
                self.current_pos_ms = pending.start_ms;
                if self.apply_transcription_result(
                    client,
                    pending.subtitle_path.as_deref(),
                    pending.start_ms,
                    worker_result.device_notice,
                ) {
                    self.current_pos_ms = pending.start_ms.saturating_add(pending.duration_ms);
                    if !self.subs_loaded {
                        let main_srt = pending
                            .subtitle_path
                            .unwrap_or_else(|| self.paths.tmp_sub.with_extension("srt"));
                        let _ = client.command(&["sub-add", main_srt.to_str().unwrap_or_default()]);
                        self.subs_loaded = true;
                    }
                    if self.config.playback.show_progress {
                        let _ = client.command(&[
                            "show-text",
                            &format!("STT: {}", Self::format_progress(self.current_pos_ms)),
                        ]);
                    }
                }
            }
            Err(MpvSttError::SttCancelled | MpvSttError::AudioExtractionCancelled) => {
                debug!("Transcription job cancelled");
                self.paths.cleanup_intermediate_subs();
            }
            Err(err) => {
                error!("Transcription job failed: {err}");
                let msg = format!("STT failed: {err}");
                let _ = client.command(&["show-text", &msg, "4000"]);
                self.stop_transcription();
            }
        }
    }

    fn start_transcription(&mut self, client: &mut Handle) {
        debug!("Starting transcription");
        self.transcription_complete = false;
        // Get current position
        let time_pos: f64 = client.get_property("time-pos").unwrap_or(0.0);
        self.current_pos_ms = (time_pos * 1000.0) as u64;
        self.last_playback_pos_ms = Some(self.current_pos_ms);
        trace!("Current playback position: {}ms", self.current_pos_ms);

        // Check if network stream - use multiple detection methods
        let is_network = self.detect_network_stream(client);

        if is_network {
            // Network stream mode
            debug!("Detected network stream, entering network mode");
            let _ = client.command(&["show-text", "STT: Starting network stream transcription..."]);

            // Enable caching
            let _ = client.set_property("cache", true);

            // Set demuxer max bytes if configured (for better lookahead caching)
            if let Some(max_bytes) = self.config.network.demuxer_max_bytes {
                debug!("Setting demuxer-max-bytes to {} bytes", max_bytes);
                let _ = client.set_property("demuxer-max-bytes", max_bytes);
            }

            self.mode = Some(ProcessingMode::Network);
            self.network_cache = None;
            let chunk_size = self.network_chunk_size();
            self.current_pos_ms -= self.current_pos_ms % chunk_size;

            if self.config.playback.save_srt {
                if let Some(media_id) = Self::media_id_for_cache(client) {
                    if let Some(cache_paths) = self.cache_paths_for_media(&media_id) {
                        if let Some(parent) = cache_paths.subtitle_path.parent() {
                            if let Err(err) = fs::create_dir_all(parent) {
                                warn!(
                                    "Failed to create cache directory {}: {}",
                                    parent.display(),
                                    err
                                );
                            }
                        }
                        if cache_paths.subtitle_path.exists() {
                            if self.load_cached_subs(
                                &cache_paths.subtitle_path,
                                Some(&cache_paths.manifest_path),
                                self.network_chunk_size(),
                            ) {
                                let _ = client.command(&[
                                    "sub-add",
                                    cache_paths.subtitle_path.to_str().unwrap(),
                                ]);
                                self.subs_loaded = true;
                                info!(
                                    "Loaded cached subtitles from {}",
                                    cache_paths.subtitle_path.display()
                                );
                            }
                        }
                        self.network_cache = Some(cache_paths);
                    }
                }
            }

            info!(
                "Network stream mode active, current_pos: {}ms",
                self.current_pos_ms
            );
        } else {
            // Local file mode
            debug!("Detected local file, entering local mode");
            let media_path: Result<String, _> = client.get_property("path");
            let duration: Result<f64, _> = client.get_property("duration");

            if let (Ok(path), Ok(dur)) = (media_path, duration) {
                let file_length_ms = (dur * 1000.0) as u64;
                trace!("Media file: {}, duration: {}ms", path, file_length_ms);

                // Calculate subtitle path next to the video file when possible.
                // SAF content:// URIs are not writable as filesystem paths.
                let subtitle_path = if self.config.playback.save_srt {
                    Self::get_subtitle_path_for_media_uri(&path)
                        .unwrap_or_else(|| self.paths.tmp_sub.with_extension("srt"))
                } else {
                    self.paths.tmp_sub.with_extension("srt")
                };
                info!("Subtitle will be saved to: {}", subtitle_path.display());

                let _ = client.command(&["show-text", "STT: Starting local file transcription..."]);

                // Start from beginning if configured
                let chunk_size = self.local_chunk_size();
                self.current_pos_ms -= self.current_pos_ms % chunk_size;

                self.mode = Some(ProcessingMode::Local {
                    media_path: path.clone(),
                    file_length_ms,
                    subtitle_path: subtitle_path.clone(),
                });
                self.network_cache = None;

                if self.config.playback.save_srt && subtitle_path.exists() {
                    if self.load_cached_subs(&subtitle_path, None, self.local_chunk_size()) {
                        let _ = client.command(&["sub-add", subtitle_path.to_str().unwrap()]);
                        self.subs_loaded = true;
                        info!("Loaded cached subtitles from {}", subtitle_path.display());
                    }
                }

                // Create initial subtitles if this chunk hasn't been processed.
                if !self.is_chunk_processed(self.current_pos_ms) {
                    let remaining_ms = file_length_ms.saturating_sub(self.current_pos_ms);
                    self.chunk_dur = self.local_chunk_size().min(remaining_ms).max(1);
                    self.schedule_transcription(
                        path.clone(),
                        self.current_pos_ms,
                        self.chunk_dur,
                        Some(subtitle_path.clone()),
                    );
                }

                info!(
                    "Local file mode: {}, length: {}ms, start: {}ms",
                    path, file_length_ms, self.current_pos_ms
                );
            } else {
                self.running = false;
                self.mode = None;
                warn!("Cannot start STT: no playable media path/duration is available");
                let _ = client.command(&[
                    "show-text",
                    "STT: Please open a playable media file first",
                    "4000",
                ]);
            }
        }
    }

    /// Main processing loop - called on each event loop iteration
    fn tick(&mut self, client: &mut Handle) {
        if self.shutting_down {
            return;
        }

        if !self.running {
            if self.transcription_complete {
                let subtitle_path = match &self.mode {
                    Some(ProcessingMode::Network) => self
                        .network_cache
                        .as_ref()
                        .map(|cache| cache.subtitle_path.clone()),
                    Some(ProcessingMode::Local { subtitle_path, .. }) => {
                        Some(subtitle_path.clone())
                    }
                    None => None,
                };
                self.process_translation_results(client, subtitle_path.as_deref());
            }
            return;
        }

        if self.pending_transcription.is_some() && self.check_seek(client) {
            return;
        }
        self.poll_transcription(client);
        if !self.running || self.shutting_down {
            return;
        }

        match &self.mode {
            Some(ProcessingMode::Network) => self.tick_network(client),
            Some(ProcessingMode::Local {
                media_path,
                file_length_ms,
                subtitle_path,
            }) => {
                let media_path = media_path.clone();
                let file_length_ms = *file_length_ms;
                let subtitle_path = subtitle_path.clone();
                self.tick_local(client, &media_path, file_length_ms, &subtitle_path);
            }
            None => {}
        }
    }

    fn tick_network(&mut self, client: &mut Handle) {
        let subtitle_path = self
            .network_cache
            .as_ref()
            .map(|cache| cache.subtitle_path.clone());

        // Check for seek first. If cache isn't ready yet after a seek, we still want to update
        // `current_pos_ms` so we don't keep generating subtitles for the old position.
        if self.check_seek(client) {
            return;
        }

        // Check for completed translations from async queue
        self.process_translation_results(client, subtitle_path.as_deref());

        // Get cache end time
        let cache_end_sec: Option<f64> = client.get_property("demuxer-cache-time").ok();
        if cache_end_sec.is_none() {
            trace!("Cache not ready yet");
            return; // Cache not ready yet
        }
        let cache_end_ms = (cache_end_sec.unwrap() * 1000.0) as u64;
        let available_ms = cache_end_ms.saturating_sub(self.current_pos_ms);
        let chunk_ms = self.network_chunk_size();

        if available_ms < chunk_ms {
            trace!(
                "Waiting for more cache: need {}ms, have {}ms",
                self.current_pos_ms + chunk_ms,
                cache_end_ms
            );
            return;
        }

        // Catch-up mode: check if we're too far behind playback (always enabled)
        // Look-ahead processing for network streams (always enabled)
        // Check how far ahead playback is from processing
        if let Some(playback_pos_ms) = self.last_playback_pos_ms {
            let _ahead = if self.current_pos_ms > playback_pos_ms {
                self.current_pos_ms - playback_pos_ms
            } else {
                0
            };

            // No lookahead limit; we rely on cache availability below
        }

        if self.pending_transcription.is_some() {
            return;
        }

        while self.is_chunk_processed(self.current_pos_ms) {
            self.current_pos_ms = self.current_pos_ms.saturating_add(chunk_ms);
        }

        let chunk_end_ms = self.current_pos_ms.saturating_add(chunk_ms);
        let lookahead_limit_ms =
            chunk_ms.saturating_mul(self.config.prefetch.lookahead_chunks.max(1) as u64);
        if let Some(playback_pos_ms) = self.last_playback_pos_ms {
            let ahead_end_ms = chunk_end_ms.saturating_sub(playback_pos_ms);
            if ahead_end_ms > lookahead_limit_ms {
                trace!(
                    "Look-ahead limit reached: chunk end {}ms ahead (limit {}ms); waiting",
                    ahead_end_ms, lookahead_limit_ms
                );
                return;
            }
        }

        if chunk_end_ms > cache_end_ms {
            trace!(
                "Waiting for more cache: need {}ms, have {}ms",
                chunk_end_ms, cache_end_ms
            );
            return;
        }

        debug!("Scheduling network chunk at {}ms", self.current_pos_ms);
        self.process_chunk(client, chunk_ms, subtitle_path.as_deref());
    }

    fn tick_local(
        &mut self,
        client: &mut Handle,
        media_path: &str,
        file_length_ms: u64,
        subtitle_path: &Path,
    ) {
        // Check for seek
        if self.check_seek(client) {
            return;
        }

        // Check for completed translations from async queue
        self.process_translation_results(client, Some(subtitle_path));

        // Calculate remaining time
        let time_left = if file_length_ms > self.current_pos_ms {
            file_length_ms - self.current_pos_ms
        } else {
            0
        };

        // Adjust chunk size for last chunk
        let local_chunk_size = self.local_chunk_size();
        if time_left > 0 && time_left < local_chunk_size {
            self.chunk_dur = time_left;
        } else {
            self.chunk_dur = local_chunk_size;
        }

        if time_left > 0 {
            if self.pending_transcription.is_some() {
                return;
            }

            while self.is_chunk_processed(self.current_pos_ms) {
                self.current_pos_ms = self.current_pos_ms.saturating_add(local_chunk_size);
                if self.current_pos_ms >= file_length_ms {
                    return;
                }
            }

            let lookahead_limit_ms = local_chunk_size
                .saturating_mul(self.config.prefetch.lookahead_chunks.max(1) as u64);
            let chunk_end_ms = self.current_pos_ms.saturating_add(self.chunk_dur);
            if let Some(playback_pos_ms) = self.last_playback_pos_ms {
                let ahead_end_ms = chunk_end_ms.saturating_sub(playback_pos_ms);
                if ahead_end_ms > lookahead_limit_ms {
                    trace!(
                        "Look-ahead limit reached: chunk end {}ms ahead (limit {}ms); waiting",
                        ahead_end_ms, lookahead_limit_ms
                    );
                    return;
                }
            }

            debug!(
                "Scheduling local chunk at {}ms, remaining: {}ms",
                self.current_pos_ms, time_left
            );
            self.process_chunk_local(media_path, subtitle_path);
        } else {
            // Finished processing
            if !self.transcription_complete {
                info!("Finished processing local file");
                let msg = if self.config.playback.save_srt {
                    format!("STT: Saved subtitles to {}", subtitle_path.display())
                } else {
                    "STT: Transcription complete".to_string()
                };
                let _ = client.command(&["show-text", &msg, "5000"]);
                self.running = false;
                self.transcription_complete = true;
            }
        }
    }

    fn check_seek(&mut self, client: &mut Handle) -> bool {
        let playback_pos: Option<f64> = client.get_property("time-pos").ok();
        if let Some(pos) = playback_pos {
            let playback_pos_ms = (pos * 1000.0) as u64;
            let now = Instant::now();

            // Detect user seek by comparing against the last observed playback position.
            // IMPORTANT: `current_pos_ms` is the *processing cursor* (next chunk start), which can
            // legitimately run ahead of playback when the cache is full. Comparing playback to
            // `current_pos_ms` causes false "seek backward" detections and makes subtitles vanish.
            let Some(last_ms) = self.last_playback_pos_ms.replace(playback_pos_ms) else {
                self.last_playback_instant = Some(now);
                return false;
            };
            let last_instant = self.last_playback_instant.replace(now);

            // Avoid treating normal playback progression (or time spent inside STT/translate)
            // as a seek. Since we process in chunk units, only treat jumps of >= 1 chunk as seek.
            let chunk_size = self.active_chunk_size();
            let seek_threshold_ms = std::cmp::max(5_000, chunk_size);
            let delta_ms = playback_pos_ms.abs_diff(last_ms);
            if delta_ms < seek_threshold_ms {
                return false;
            }
            if let Some(last_instant) = last_instant {
                let elapsed_ms = now
                    .duration_since(last_instant)
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX);
                if delta_ms <= elapsed_ms.saturating_add(seek_threshold_ms) {
                    return false;
                }
            }

            let new_pos = playback_pos_ms - (playback_pos_ms % chunk_size);
            if new_pos == self.current_pos_ms {
                debug!(
                    "Seek landed within current chunk ({}ms); keeping active tasks",
                    new_pos
                );
                return false;
            }

            // Detect seek forward (user skipped ahead)
            if playback_pos_ms > last_ms {
                debug!(
                    "User seeked forward from {}ms to {}ms (delta: {}ms)",
                    last_ms, new_pos, delta_ms
                );
                let _ = client.command(&[
                    "show-text",
                    &format!("STT: Jumped to {}", Self::format_progress(new_pos)),
                    "3000",
                ]);

                // Keep existing subtitles, just update processing cursor.
                self.current_pos_ms = new_pos;
                self.cancel_translation_inflight();
                self.transcription_worker.cancel_inflight();
                self.pending_transcription = None;
                if self.is_chunk_processed(new_pos) {
                    self.enqueue_missing_translations_for_chunk(new_pos);
                }
                return true;
            }
            // Detect seek backward
            else {
                debug!(
                    "User seeked backward from {}ms to {}ms (delta: {}ms)",
                    last_ms, new_pos, delta_ms
                );
                let _ = client.command(&[
                    "show-text",
                    &format!("STT: Seeked back to {}", Self::format_progress(new_pos)),
                    "3000",
                ]);

                self.current_pos_ms = new_pos;
                self.cancel_translation_inflight();
                self.transcription_worker.cancel_inflight();
                self.pending_transcription = None;
                if self.is_chunk_processed(new_pos) {
                    self.enqueue_missing_translations_for_chunk(new_pos);
                }
                return true;
            }
        }
        false
    }

    /// Process one chunk from network cache
    fn process_chunk(
        &mut self,
        client: &mut Handle,
        chunk_ms: u64,
        subtitle_path: Option<&Path>,
    ) -> bool {
        // Dump cache
        let start_sec = self.current_pos_ms as f64 / 1000.0;
        let end_sec = (self.current_pos_ms + chunk_ms) as f64 / 1000.0;
        trace!("Dumping cache from {}s to {}s", start_sec, end_sec);

        let dump_result = client.command(&[
            "dump-cache",
            &start_sec.to_string(),
            &end_sec.to_string(),
            self.paths.tmp_cache.to_str().unwrap(),
        ]);

        if dump_result.is_err() {
            error!("dump-cache failed");
            return false;
        }

        self.schedule_transcription(
            self.paths.tmp_cache.to_string_lossy().into_owned(),
            0,
            chunk_ms,
            subtitle_path.map(Path::to_path_buf),
        )
    }

    /// Process one chunk from local file
    fn process_chunk_local(&mut self, media_path: &str, subtitle_path: &Path) -> bool {
        self.schedule_transcription(
            media_path.to_string(),
            self.current_pos_ms,
            self.chunk_dur,
            Some(subtitle_path.to_path_buf()),
        )
    }

    /// Apply a completed worker result on the mpv event thread.
    fn apply_transcription_result(
        &mut self,
        client: &mut Handle,
        subtitle_path: Option<&Path>,
        chunk_start_ms: u64,
        device_notice: Option<SttDeviceNotice>,
    ) -> bool {
        let tmp_sub_prefix = self.paths.tmp_sub.to_string_lossy().to_string();
        let append_path = format!("{}_append", &tmp_sub_prefix);
        let main_srt = subtitle_path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.paths.tmp_sub.with_extension("srt"));
        self.show_device_notice(client, device_notice);

        // Offset timestamps
        let append_srt = format!("{}.srt", append_path);
        let offset_srt = format!("{}_append_offset.srt", &tmp_sub_prefix);

        if let Ok(meta) = std::fs::metadata(&append_srt) {
            if meta.len() == 0 {
                info!(
                    "Chunk starting at {}ms produced no subtitles; skipping merge",
                    chunk_start_ms
                );
                self.mark_chunk_processed(chunk_start_ms);
                self.paths.cleanup_intermediate_subs();
                return true;
            }
        }

        if let Err(e) = crate::srt::offset_srt_file(&append_srt, &offset_srt, chunk_start_ms as i64)
        {
            error!("SRT offset failed: {}", e);
            return false;
        }

        // Add original subtitles first so recognition updates immediately
        let srt_file = match SrtFile::parse(&offset_srt) {
            Ok(srt) => srt,
            Err(_) => return false,
        };
        self.subtitle_manager.add_from_srt(&srt_file);
        self.mark_chunk_processed(chunk_start_ms);

        let mut pending_tasks = Vec::new();
        let mut already_translated = 0usize;

        for entry in &srt_file.entries {
            let original = entry.text.trim();
            if original.is_empty() {
                continue;
            }
            let start_ms = Self::timestamp_to_millis(entry.start_time);
            if SubtitleManager::text_has_translation(&entry.text) {
                already_translated += 1;
                continue;
            }

            pending_tasks.push(TranslationTask {
                start_ms,
                text: entry.text.clone(),
            });
        }

        if !self.save_subs(client, &main_srt) {
            return false;
        }

        // Translate using async translation queue (Ctrl+Shift+t toggles translate_enabled)
        if self.translate_enabled && !pending_tasks.is_empty() {
            if let Some(ref queue) = self.async_translation_queue {
                trace!("Submitting subtitles to async translation queue");
                for task in pending_tasks {
                    queue.submit(task);
                }
                debug!(
                    "Submitted {} entries to async translation (already translated: {})",
                    srt_file.entries.len(),
                    already_translated
                );
            }
        } else if already_translated > 0 {
            debug!(
                "All {} entries already had translations",
                already_translated
            );
        }

        // Keep only the main subtitle file on disk during playback to reduce clutter.
        // The `_append*` files are per-chunk intermediates and will be regenerated each chunk.
        self.paths.cleanup_intermediate_subs();

        debug!(
            "Processed chunk at {}ms, total subs: {}",
            self.current_pos_ms,
            self.subtitle_manager.len()
        );
        true
    }

    fn show_device_notice(&mut self, client: &mut Handle, device_notice: Option<SttDeviceNotice>) {
        let Some(notice) = device_notice else {
            return;
        };

        let mut msg = format!("STT device: {}", notice.effective);
        if notice.effective.is_gpu() {
            msg.push_str(&format!(" (gpu_device: {})", notice.gpu_device));
        }
        if notice.effective != notice.requested {
            msg.push_str(&format!(
                " (fallback from {}: {})",
                notice.requested, notice.reason
            ));
        }

        let _ = client.command(&["show-text", &msg, "3000"]);
        info!("STT device notice: {}", msg);
    }

    /// Process completed translation results from async queue
    fn process_translation_results(&mut self, client: &mut Handle, subtitle_path: Option<&Path>) {
        if let Some(ref queue) = self.async_translation_queue {
            let results = queue.try_recv_results();
            if !results.is_empty() {
                debug!("Received {} translation results", results.len());

                let main_srt = subtitle_path
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| self.paths.tmp_sub.with_extension("srt"));

                // Update subtitles as each translation completes.
                for result in results {
                    self.translation_cache.insert(
                        result.start_ms,
                        (result.original.clone(), result.translated.clone()),
                    );
                    self.subtitle_manager
                        .update_translation(result.start_ms, &result.translated);
                    let _ = self.save_subs(client, &main_srt);
                }
            }
        }
    }

    fn timestamp_to_millis(ts: crate::srt::Timestamp) -> u32 {
        let (h, m, s, ms) = ts.get();
        crate::srt::Timestamp::convert_to_milliseconds(h, m, s, ms)
    }

    fn save_subs(&mut self, client: &mut Handle, main_srt: &Path) -> bool {
        if let Err(e) = self.subtitle_manager.save_to_file(main_srt) {
            error!("Failed to save subtitles: {}", e);
            return false;
        }
        if self.subs_loaded {
            let _ = client.command(&["sub-reload"]);
        }
        self.save_cache_manifest_if_needed();
        true
    }

    /// Stop the current media/transcription session while keeping the plugin
    /// alive. This path is used by the toggle, EndFile, completed media and
    /// recoverable STT errors, so it must remain restartable.
    fn stop_transcription(&mut self) {
        debug!("Stopping current transcription and cleaning up session state");

        self.running = false;
        self.transcription_worker.cancel_inflight();
        self.pending_transcription = None;

        // Cancel tasks belonging to this media, but keep the worker alive so
        // Ctrl+Shift+S and the next file can start a fresh session.
        if let Some(queue) = self.async_translation_queue.as_ref() {
            queue.cancel_inflight();
        }

        self.paths.cleanup();
        self.subtitle_manager.clear();
        self.translation_cache.clear();
        self.processed_chunks.clear();
        self.network_cache = None;
        self.subs_loaded = false;
        self.current_pos_ms = 0;
        self.last_playback_pos_ms = None;
        self.last_playback_instant = None;
        self.mode = None;
        self.transcription_complete = false;
    }

    /// Permanently shut down resources immediately before the mpv client is
    /// destroyed. Unlike `stop_transcription`, this is terminal.
    fn shutdown(&mut self) {
        if self.shutting_down {
            return;
        }

        self.shutting_down = true;
        self.stop_transcription();
        self.transcription_worker.shutdown();
        if let Some(mut queue) = self.async_translation_queue.take() {
            queue.shutdown();
        }
    }

    fn format_progress(ms: u64) -> String {
        let seconds = ms / 1000;
        let minutes = seconds / 60;
        let hours = minutes / 60;

        let seconds = seconds % 60;
        let minutes = minutes % 60;
        let millis = ms % 1000;

        format!("{:02}:{:02}:{:02}.{:03}", hours, minutes, seconds, millis)
    }

    fn is_chunk_processed(&self, start_ms: u64) -> bool {
        self.processed_chunks.contains(&start_ms)
    }

    fn mark_chunk_processed(&mut self, start_ms: u64) {
        self.processed_chunks.insert(start_ms);
    }

    fn media_id_for_cache(client: &mut Handle) -> Option<String> {
        if let Ok(id) = client.get_property::<String>("stream-open-filename") {
            if !id.trim().is_empty() {
                return Some(id);
            }
        }
        if let Ok(id) = client.get_property::<String>("path") {
            if !id.trim().is_empty() {
                return Some(id);
            }
        }
        None
    }

    fn cache_root_dir() -> Option<PathBuf> {
        let base = directories::BaseDirs::new()?;
        Some(
            base.config_dir()
                .join("mpv")
                .join("mpv_stt_plugin_rs_cache"),
        )
    }

    fn cache_paths_for_media(&self, media_id: &str) -> Option<CachePaths> {
        let dir = Self::cache_root_dir()?;
        let hash = Self::fnv1a_hash64(media_id);
        let stem = format!("{:016x}", hash);
        Some(CachePaths {
            subtitle_path: dir.join(format!("{stem}.srt")),
            manifest_path: dir.join(format!("{stem}.json")),
        })
    }

    fn fnv1a_hash64(input: &str) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;
        let mut hash = FNV_OFFSET;
        for byte in input.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    fn load_cached_subs(
        &mut self,
        srt_path: &Path,
        manifest_path: Option<&Path>,
        chunk_size_ms: u64,
    ) -> bool {
        if !srt_path.exists() {
            return false;
        }

        let srt_file = match SrtFile::parse(srt_path) {
            Ok(srt) => srt,
            Err(err) => {
                warn!(
                    "Failed to parse cached subtitles {}: {}",
                    srt_path.display(),
                    err
                );
                return false;
            }
        };

        self.subtitle_manager.clear();
        self.translation_cache.clear();
        self.processed_chunks.clear();
        self.subtitle_manager.add_from_srt(&srt_file);

        let chunk_size = chunk_size_ms.max(1);
        for entry in &srt_file.entries {
            let start_ms = Self::timestamp_to_millis(entry.start_time) as u64;
            let chunk_start = start_ms - (start_ms % chunk_size);
            self.processed_chunks.insert(chunk_start);
        }

        if let Some(path) = manifest_path {
            if let Some(manifest) = self.load_cache_manifest(path) {
                if manifest.chunk_size_ms == chunk_size {
                    for chunk in manifest.processed_chunks {
                        self.processed_chunks.insert(chunk);
                    }
                }
                for entry in manifest.translations {
                    if !entry.translated.trim().is_empty() {
                        self.translation_cache
                            .insert(entry.start_ms, (entry.original, entry.translated));
                    }
                }
            }
        }

        true
    }

    fn load_cache_manifest(&self, path: &Path) -> Option<CacheManifest> {
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn save_cache_manifest_if_needed(&self) {
        let Some(cache) = &self.network_cache else {
            return;
        };

        let mut processed_chunks: Vec<u64> = self.processed_chunks.iter().copied().collect();
        processed_chunks.sort_unstable();

        let translations = self
            .translation_cache
            .iter()
            .map(|(start_ms, (original, translated))| TranslationCacheEntry {
                start_ms: *start_ms,
                original: original.clone(),
                translated: translated.clone(),
            })
            .collect();

        let manifest = CacheManifest {
            chunk_size_ms: self.network_chunk_size(),
            processed_chunks,
            translations,
        };

        let content = match serde_json::to_string(&manifest) {
            Ok(data) => data,
            Err(err) => {
                warn!("Failed to serialize cache manifest: {}", err);
                return;
            }
        };

        if let Err(err) = fs::write(&cache.manifest_path, content) {
            warn!(
                "Failed to write cache manifest {}: {}",
                cache.manifest_path.display(),
                err
            );
        }
    }

    /// Detect if current media is a network stream
    fn detect_network_stream(&self, client: &mut Handle) -> bool {
        // Method 1: Check path/filename for http/https URLs
        if let Ok(path) = client.get_property::<String>("path") {
            debug!("Checking path for network stream: {}", path);
            if path.starts_with("http://") || path.starts_with("https://") {
                debug!("Detected network stream by URL prefix");
                return true;
            }
        }

        // Method 2: Check stream-open-filename
        if let Ok(filename) = client.get_property::<String>("stream-open-filename") {
            debug!("Checking stream-open-filename: {}", filename);
            if filename.starts_with("http://") || filename.starts_with("https://") {
                debug!("Detected network stream by stream-open-filename");
                return true;
            }
        }

        // Method 3: Check demuxer-via-network property
        if let Ok(via_network) = client.get_property::<String>("demuxer-via-network") {
            debug!("demuxer-via-network: {}", via_network);
            if via_network == "yes" {
                debug!("Detected network stream by demuxer-via-network");
                return true;
            }
        }

        debug!("Not detected as network stream, treating as local file");
        false
    }

    /// Get subtitle path for a media file (same directory, same name, .srt extension)
    fn get_subtitle_path_for_media(media_path: &str) -> PathBuf {
        let path = Path::new(media_path);
        if let Some(stem) = path.file_stem() {
            if let Some(parent) = path.parent() {
                return parent.join(format!("{}.srt", stem.to_string_lossy()));
            }
        }
        // Fallback: just append .srt
        PathBuf::from(format!("{}.srt", media_path))
    }

    /// Try to map a media path/URI to a writable filesystem subtitle path.
    /// Returns None for non-filesystem URIs like content://.
    fn get_subtitle_path_for_media_uri(media_path: &str) -> Option<PathBuf> {
        if let Some(rest) = media_path.strip_prefix("file://") {
            return Some(Self::get_subtitle_path_for_media(rest));
        }
        if media_path.contains("://") {
            return None;
        }
        Some(Self::get_subtitle_path_for_media(media_path))
    }
}

impl Drop for PluginState {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// MPV C plugin entry point
#[unsafe(no_mangle)]
pub extern "C" fn mpv_open_cplugin(handle: *mut mpv_handle) -> std::os::raw::c_int {
    #[cfg(target_os = "android")]
    init_panic_logger();

    let result = std::panic::catch_unwind(|| {
        init_logger();

        let client = Handle::from_ptr(handle);

        info!("mpv_stt_plugin_rs Rust plugin initializing...");

        // Print welcome message
        let _ = client.command(&["show-text", "mpv_stt_plugin_rs Rust plugin loaded!", "3000"]);
        info!("Plugin loaded, client name: {}", client.name());

        // Initialize plugin state with configuration
        let env_cfg_path = Config::config_path_from_env();
        let default_cfg_path = Config::default_config_path();
        let config = Config::load();

        info!(
            "Config source: env MPV_STT_PLUGIN_RS_CONFIG={:?}, default={:?}",
            env_cfg_path.as_ref().map(|p| p.display().to_string()),
            default_cfg_path.as_ref().map(|p| p.display().to_string())
        );
        // Deliberately log only non-secret fields. The full Config debug value
        // contains API/encryption/auth keys and must never be written to IINA's
        // logs.
        info!(
            "Effective config: stt.backend={}, stt.model={}, stt.language={}, translate.backend={}, translate.languages={}->{}, chunks={}ms/{}ms, auto_start={}, save_srt={}",
            config.stt.backend,
            match config.stt.backend {
                crate::config::BackendKind::Ferrum => config
                    .stt
                    .ferrum
                    .as_ref()
                    .map(|cfg| cfg.model.as_str())
                    .unwrap_or("<missing>"),
                crate::config::BackendKind::OpenAi => config
                    .stt
                    .openai
                    .as_ref()
                    .map(|cfg| cfg.model.as_str())
                    .unwrap_or("<missing>"),
            },
            match config.stt.backend {
                crate::config::BackendKind::Ferrum => config
                    .stt
                    .ferrum
                    .as_ref()
                    .and_then(|cfg| cfg.language.as_deref())
                    .unwrap_or("auto"),
                crate::config::BackendKind::OpenAi => config
                    .stt
                    .openai
                    .as_ref()
                    .and_then(|cfg| cfg.language.as_deref())
                    .unwrap_or("auto"),
            },
            config.translate.backend,
            config.translate.from_lang,
            config.translate.to_lang,
            config.chunk.local_ms,
            config.chunk.network_ms,
            config.playback.auto_start,
            config.playback.save_srt,
        );
        let auto_start = config.playback.auto_start;
        let mut state = match PluginState::new(config) {
            Ok(state) => state,
            Err(err) => {
                error!("Failed to initialize STT plugin: {err}");
                let _ = client.command(&[
                    "show-text",
                    &format!("STT plugin initialization failed: {err}"),
                    "8000",
                ]);
                return -1;
            }
        };

        // Target the numeric client ID rather than a filename-derived name.
        // IINA can create multiple mpv cores and mpv may suffix duplicate
        // client names; IDs are unambiguous for the lifetime of this client.
        let client_name = client.name().to_string();
        let client_target = format!("@{}", client.id());

        // Use a forced section so IINA/default input bindings cannot silently
        // shadow the plugin controls. Users explicitly chose these shortcuts.
        let key_bindings = key_binding_section(&client_target);
        let section_name = format!("{}-input", client_name);

        if let Err(err) = client.command(&["define-section", &section_name, &key_bindings, "force"])
        {
            error!("Failed to define input section {section_name}: {err}");
            let _ = client.command(&[
                "show-text",
                "STT plugin: failed to register shortcuts (see log)",
                "5000",
            ]);
        } else if let Err(err) = client.command(&["enable-section", &section_name]) {
            error!("Failed to enable input section {section_name}: {err}");
            let _ = client.command(&[
                "show-text",
                "STT plugin: failed to enable shortcuts (see log)",
                "5000",
            ]);
        } else {
            info!(
                "Registered forced shortcuts for client {} (target {}): Ctrl+Shift+S/T/C",
                client_name, client_target
            );
        }

        // Set auto-start flag (will start after file loads)
        if auto_start {
            info!("Auto-start enabled, waiting for file to load...");
            state.pending_auto_start = true;
        }

        // If a file is already loaded when the plugin is attached (e.g., script reload),
        // try to start immediately instead of waiting for the next FileLoaded event.
        if auto_start && !state.running {
            if client.get_property::<f64>("duration").is_ok() {
                debug!("Auto-start: media already loaded, starting immediately");
                state.file_loaded = true;
                state.pending_auto_start = false;
                state.running = true;
                state.start_transcription(client);
            }
        }

        // Main event loop with short timeout for continuous processing
        loop {
            // Use 0.1 second timeout to allow continuous processing
            match client.wait_event(0.1) {
                Event::Shutdown => {
                    info!("Shutting down...");
                    state.shutdown();
                    info!("Shutdown complete");
                    return 0;
                }
                Event::ClientMessage(msg) => {
                    if state.shutting_down {
                        continue;
                    }
                    let args = msg.args();
                    if let Some(command) = ControlCommand::from_client_message(&args) {
                        match command {
                            ControlCommand::ToggleStt => {
                                debug!("Toggling STT...");
                                state.toggle_stt(client);
                            }
                            ControlCommand::ToggleTranslate => {
                                debug!("Toggling translation...");
                                state.toggle_translate(client);
                            }
                            ControlCommand::ClearCache => {
                                debug!("Clearing subtitle cache...");
                                state.clear_cache(client);
                            }
                        }
                    }
                }
                Event::StartFile(_) => {
                    if state.shutting_down {
                        continue;
                    }
                    debug!("StartFile event received");
                    // Be defensive if a frontend switches files without an
                    // EndFile event reaching this client.
                    if state.running || state.mode.is_some() {
                        state.stop_transcription();
                    }
                    state.file_loaded = false;
                    state.pending_auto_start = state.config.playback.auto_start;
                }
                Event::FileLoaded => {
                    if state.shutting_down {
                        continue;
                    }
                    debug!("File loaded event received");
                    state.file_loaded = true;

                    // Trigger auto-start if pending
                    if state.pending_auto_start && !state.running {
                        info!("Auto-starting STT after file load");
                        state.pending_auto_start = false;
                        state.running = true;
                        state.start_transcription(client);
                    }
                }
                Event::PlaybackRestart => {
                    if state.shutting_down {
                        continue;
                    }
                    debug!("Playback restart event received");

                    // Also trigger auto-start on playback restart (backup mechanism)
                    if state.pending_auto_start && !state.running && state.file_loaded {
                        info!("Auto-starting STT after playback restart");
                        state.pending_auto_start = false;
                        state.running = true;
                        state.start_transcription(client);
                    }

                    state.tick(client);
                }
                Event::EndFile(_) => {
                    if !state.shutting_down && (state.running || state.mode.is_some()) {
                        state.stop_transcription();
                    }
                    state.file_loaded = false; // Reset for next file
                }
                Event::None => {
                    if state.shutting_down {
                        continue;
                    }
                    // Timeout - use this to tick the processing
                    state.tick(client);
                }
                _ => {
                    if state.shutting_down {
                        continue;
                    }
                    // Other events - still tick
                    state.tick(client);
                }
            }
        }
    });

    if let Err(err) = result {
        #[cfg(target_os = "android")]
        log_android_error(&format!("mpv_open_cplugin panicked: {:?}", err));
        #[cfg(not(target_os = "android"))]
        eprintln!("mpv_open_cplugin panicked: {:?}", err);
        return -1;
    }

    0
}

#[cfg(target_os = "android")]
fn init_panic_logger() {
    std::panic::set_hook(Box::new(|info| {
        log_android_error(&format!("panic: {}", info));
    }));
}

#[cfg(target_os = "android")]
fn log_android_error(message: &str) {
    const ANDROID_LOG_ERROR: libc::c_int = 6;
    let tag = CString::new("mpv_stt_plugin_rs").unwrap_or_default();
    let msg = CString::new(message).unwrap_or_default();
    unsafe {
        __android_log_write(ANDROID_LOG_ERROR, tag.as_ptr(), msg.as_ptr());
    }
}

#[cfg(target_os = "android")]
unsafe extern "C" {
    fn __android_log_write(
        prio: libc::c_int,
        tag: *const libc::c_char,
        text: *const libc::c_char,
    ) -> libc::c_int;
}

fn init_logger() {
    // Set MPV_STT_PLUGIN_RS_LOG environment variable to control log level (e.g., MPV_STT_PLUGIN_RS_LOG=debug)
    #[cfg(target_os = "android")]
    {
        use log::LevelFilter;

        let level = std::env::var("MPV_STT_PLUGIN_RS_LOG")
            .ok()
            .and_then(|s| {
                s.parse::<LevelFilter>()
                    .or_else(|_| s.to_lowercase().parse())
                    .ok()
            })
            .unwrap_or(LevelFilter::Info);

        let config = android_logger::Config::default()
            .with_tag("mpv_stt_plugin_rs")
            .with_max_level(level);
        let _ = android_logger::init_once(config);
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = env_logger::Builder::from_env(
            env_logger::Env::new().filter_or("MPV_STT_PLUGIN_RS_LOG", "info"),
        )
        .format_timestamp_millis()
        .try_init();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    #[test]
    fn control_messages_support_all_shortcuts_and_legacy_shape() {
        assert_eq!(
            ControlCommand::from_client_message(&["toggle-stt"]),
            Some(ControlCommand::ToggleStt)
        );
        assert_eq!(
            ControlCommand::from_client_message(&["toggle-translate"]),
            Some(ControlCommand::ToggleTranslate)
        );
        assert_eq!(
            ControlCommand::from_client_message(&["clear-cache"]),
            Some(ControlCommand::ClearCache)
        );
        assert_eq!(
            ControlCommand::from_client_message(&["legacy-route", "toggle-translate"]),
            Some(ControlCommand::ToggleTranslate)
        );
        assert_eq!(ControlCommand::from_client_message(&["unknown"]), None);
    }

    #[test]
    fn key_section_targets_the_exact_mpv_client() {
        let section = key_binding_section("@42");
        assert_eq!(section.lines().count(), KEY_BINDINGS.len());
        for (key, command) in KEY_BINDINGS {
            assert!(
                section.contains(&format!("{key} script-message-to @42 {command}")),
                "missing {key}/{command} binding in {section:?}"
            );
        }
    }

    #[test]
    fn stopping_a_session_does_not_permanently_shutdown_the_plugin() {
        let mut state = PluginState::new(Config::default()).unwrap();
        state.running = true;
        state.mode = Some(ProcessingMode::Network);

        state.stop_transcription();

        assert!(!state.running);
        assert!(!state.shutting_down);
        assert!(state.mode.is_none());
        assert!(
            state.async_translation_queue.is_some(),
            "the translation worker must remain available for the next start"
        );

        // A terminal shutdown is deliberately separate and idempotent.
        state.shutdown();
        state.shutdown();
        assert!(state.shutting_down);
        assert!(state.async_translation_queue.is_none());
    }

    #[test]
    fn transcription_worker_cancels_a_blocked_stt_request_promptly() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (request_seen_tx, request_seen_rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut byte = [0u8; 1];
            let _ = stream.read(&mut byte);
            let _ = request_seen_tx.send(());
            thread::sleep(Duration::from_secs(2));
        });

        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.wav");
        let output_wav = temp.path().join("chunk.wav");
        let output_prefix = temp.path().join("subs_append");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&input, spec).unwrap();
        for _ in 0..1_600 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();

        let mut config = Config::default();
        let openai = config.stt.openai.as_mut().unwrap();
        openai.server_addr = format!("http://{addr}");
        openai.timeout_ms = 30_000;
        openai.max_retry = 1;
        let audio = AudioExtractor::default().with_ffmpeg_timeout(5_000);
        let stt = SttRunner::from_config(&config.stt).unwrap();
        let mut worker = TranscriptionWorker::new(audio, stt);
        worker
            .submit(
                input.to_string_lossy().into_owned(),
                0,
                100,
                output_wav,
                output_prefix,
            )
            .expect("failed to submit transcription job");
        request_seen_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("STT worker never started the request");

        let started = Instant::now();
        worker.shutdown();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "worker shutdown waited for the blocked STT request: {:?}",
            started.elapsed()
        );
        assert!(worker.worker_handle.is_none());
    }
}
