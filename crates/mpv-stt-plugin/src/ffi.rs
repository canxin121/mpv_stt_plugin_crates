use crate::audio::AudioExtractor;
#[cfg(any(feature = "stt_local_cpu", feature = "stt_local_cuda"))]
use crate::stt::LocalModelConfig;
#[cfg(any(feature = "stt_local_cpu", feature = "stt_local_cuda"))]
use crate::stt::{SttBackend, SttRunner};
use crate::translate::{Translator, TranslatorConfig};
use log::{debug, error};
use mpv_stt_srt;
use parking_lot::Mutex;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::OnceLock;

// Global state for configuration
static AUDIO_EXTRACTOR: OnceLock<Mutex<AudioExtractor>> = OnceLock::new();
#[cfg(any(feature = "stt_local_cpu", feature = "stt_local_cuda"))]
static WHISPER_RUNNER: OnceLock<Mutex<Option<SttRunner>>> = OnceLock::new();
static TRANSLATOR: OnceLock<Mutex<Option<Translator>>> = OnceLock::new();

fn audio_extractor() -> &'static Mutex<AudioExtractor> {
    AUDIO_EXTRACTOR.get_or_init(|| Mutex::new(AudioExtractor::default()))
}

#[cfg(any(feature = "stt_local_cpu", feature = "stt_local_cuda"))]
fn whisper_runner() -> &'static Mutex<Option<SttRunner>> {
    WHISPER_RUNNER.get_or_init(|| Mutex::new(None))
}

fn translator_state() -> &'static Mutex<Option<Translator>> {
    TRANSLATOR.get_or_init(|| Mutex::new(None))
}

/// Helper to convert C string to Rust String
unsafe fn c_str_to_string(c_str: *const c_char) -> Option<String> {
    if c_str.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(c_str) }
        .to_str()
        .ok()
        .map(|s| s.to_string())
}

/// Helper to convert Rust String to C string (caller must free)
fn string_to_c_str(s: String) -> *mut c_char {
    CString::new(s).unwrap_or_default().into_raw()
}

/// Initialize speech-to-text configuration (local model backends)
#[cfg(any(feature = "stt_local_cpu", feature = "stt_local_cuda"))]
#[unsafe(no_mangle)]
pub extern "C" fn mpv_stt_plugin_rs_stt_init(
    model_path: *const c_char,
    threads: u8,
    language: *const c_char,
    _inference_device: i32,
    gpu_device: i32,
    flash_attn: bool,
) -> i32 {
    unsafe {
        let model_path = match c_str_to_string(model_path) {
            Some(s) => s,
            None => return -1,
        };

        let language = c_str_to_string(language).unwrap_or_else(|| "auto".to_string());

        let config = LocalModelConfig::new(model_path)
            .with_threads(threads)
            .with_language(language)
            .with_gpu_device(gpu_device)
            .with_flash_attn(flash_attn);

        let runner = SttRunner::new(config);
        *whisper_runner().lock() = Some(runner);
        debug!("STT (local) initialized via FFI");
        0
    }
}

/// Initialize Translator configuration (builtin Google Translate only)
#[unsafe(no_mangle)]
pub extern "C" fn translator_init(from_lang: *const c_char, to_lang: *const c_char) -> i32 {
    unsafe {
        let from_lang = c_str_to_string(from_lang).unwrap_or_else(|| "auto".to_string());
        let to_lang = c_str_to_string(to_lang).unwrap_or_else(|| "en".to_string());

        let config = TranslatorConfig::new(from_lang, to_lang);
        let translator = Translator::new(config);
        *translator_state().lock() = Some(translator);
        debug!("Translator initialized via FFI (builtin Google Translate)");
        0
    }
}

/// Extract audio segment from media file
#[unsafe(no_mangle)]
pub extern "C" fn extract_audio(
    input_path: *const c_char,
    output_path: *const c_char,
    start_ms: u64,
    duration_ms: u64,
) -> i32 {
    unsafe {
        let input = match c_str_to_string(input_path) {
            Some(s) => s,
            None => return -1,
        };

        let output = match c_str_to_string(output_path) {
            Some(s) => s,
            None => return -1,
        };

        let extractor = audio_extractor().lock();
        match extractor.extract_audio_segment(&input, &output, start_ms, duration_ms) {
            Ok(_) => 0,
            Err(e) => {
                error!("Audio extraction error: {}", e);
                -1
            }
        }
    }
}

/// Run Whisper transcription
#[cfg(any(feature = "stt_local_cpu", feature = "stt_local_cuda"))]
#[unsafe(no_mangle)]
pub extern "C" fn stt_transcribe(
    audio_path: *const c_char,
    output_prefix: *const c_char,
    duration_ms: u64,
) -> i32 {
    unsafe {
        let audio = match c_str_to_string(audio_path) {
            Some(s) => s,
            None => return -1,
        };

        let output = match c_str_to_string(output_prefix) {
            Some(s) => s,
            None => return -1,
        };

        let mut runner_guard = whisper_runner().lock();
        let runner = match runner_guard.as_mut() {
            Some(r) => r,
            None => {
                error!("Whisper not initialized");
                return -1;
            }
        };

        match runner.transcribe(&audio, &output, duration_ms) {
            Ok(_) => 0,
            Err(e) => {
                error!("STT transcription error: {}", e);
                -1
            }
        }
    }
}

/// Translate text
#[unsafe(no_mangle)]
pub extern "C" fn translate_text(text: *const c_char) -> *mut c_char {
    unsafe {
        let text_str = match c_str_to_string(text) {
            Some(s) => s,
            None => return std::ptr::null_mut(),
        };

        let translator_guard = translator_state().lock();
        let translator = match translator_guard.as_ref() {
            Some(t) => t,
            None => return std::ptr::null_mut(),
        };

        match translator.translate(&text_str) {
            Ok(result) => string_to_c_str(result),
            Err(e) => {
                error!("Translation error: {}", e);
                std::ptr::null_mut()
            }
        }
    }
}

/// Translate SRT file
#[unsafe(no_mangle)]
pub extern "C" fn translate_srt(input_path: *const c_char, output_path: *const c_char) -> i32 {
    unsafe {
        let input = match c_str_to_string(input_path) {
            Some(s) => s,
            None => return -1,
        };

        let output = match c_str_to_string(output_path) {
            Some(s) => s,
            None => return -1,
        };

        let translator_guard = translator_state().lock();
        let translator = match translator_guard.as_ref() {
            Some(t) => t,
            None => {
                error!("Translator not initialized");
                return -1;
            }
        };

        match translator.translate_srt_file(&input, &output) {
            Ok(_) => 0,
            Err(e) => {
                error!("SRT translation error: {}", e);
                -1
            }
        }
    }
}

/// Offset SRT file timestamps
#[unsafe(no_mangle)]
pub extern "C" fn offset_srt(
    input_path: *const c_char,
    output_path: *const c_char,
    offset_ms: i64,
) -> i32 {
    unsafe {
        let input = match c_str_to_string(input_path) {
            Some(s) => s,
            None => return -1,
        };

        let output = match c_str_to_string(output_path) {
            Some(s) => s,
            None => return -1,
        };

        match mpv_stt_srt::offset_srt_file(&input, &output, offset_ms) {
            Ok(_) => 0,
            Err(e) => {
                error!("SRT offset error: {}", e);
                -1
            }
        }
    }
}

/// Free a C string allocated by this library
#[unsafe(no_mangle)]
pub extern "C" fn free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            let _ = CString::from_raw(s);
        }
    }
}
