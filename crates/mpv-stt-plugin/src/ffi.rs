use crate::audio::AudioExtractor;
use crate::config::TranslateBackendKind;
use crate::translate::{Translator, TranslatorConfig};
use log::{debug, error};
use mpv_stt_srt;
use parking_lot::Mutex;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::OnceLock;

// Global state for configuration
static AUDIO_EXTRACTOR: OnceLock<Mutex<AudioExtractor>> = OnceLock::new();
static TRANSLATOR: OnceLock<Mutex<Option<Translator>>> = OnceLock::new();

fn audio_extractor() -> &'static Mutex<AudioExtractor> {
    AUDIO_EXTRACTOR.get_or_init(|| Mutex::new(AudioExtractor::default()))
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

/// Initialize Translator configuration (points at the default remote
/// DeepL-compatible server on 127.0.0.1:8000, no API key).
#[unsafe(no_mangle)]
pub extern "C" fn translator_init(from_lang: *const c_char, to_lang: *const c_char) -> i32 {
    unsafe {
        let from_lang = c_str_to_string(from_lang).unwrap_or_else(|| "auto".to_string());
        let to_lang = c_str_to_string(to_lang).unwrap_or_else(|| "en".to_string());

        let config = TranslatorConfig::new(from_lang, to_lang);
        let translator = Translator::new(config);
        *translator_state().lock() = Some(translator);
        debug!("Translator initialized via FFI (default remote DeepL-compatible server)");
        0
    }
}

/// Initialize Translator configuration with an explicit remote server and
/// optional API key. Null pointers fall back to defaults.
#[unsafe(no_mangle)]
pub extern "C" fn translator_init_remote(
    from_lang: *const c_char,
    to_lang: *const c_char,
    server_addr: *const c_char,
    api_key: *const c_char,
) -> i32 {
    unsafe {
        let from_lang = c_str_to_string(from_lang).unwrap_or_else(|| "auto".to_string());
        let to_lang = c_str_to_string(to_lang).unwrap_or_else(|| "en".to_string());
        let server_addr =
            c_str_to_string(server_addr).unwrap_or_else(|| "http://127.0.0.1:8000".to_string());
        let api_key = c_str_to_string(api_key).unwrap_or_default();

        let config = TranslatorConfig::new(from_lang, to_lang)
            .with_server_addr(server_addr)
            .with_api_key(api_key);
        let translator = Translator::new(config);
        *translator_state().lock() = Some(translator);
        debug!("Translator initialized via FFI (remote DeepL-compatible server)");
        0
    }
}

/// Initialize Translator configuration for the LibreTranslate protocol with an
/// explicit remote server and optional API key (sent in the body as `api_key`).
/// Null pointers fall back to defaults.
#[unsafe(no_mangle)]
pub extern "C" fn translator_init_libretranslate(
    from_lang: *const c_char,
    to_lang: *const c_char,
    server_addr: *const c_char,
    api_key: *const c_char,
) -> i32 {
    unsafe {
        let from_lang = c_str_to_string(from_lang).unwrap_or_else(|| "auto".to_string());
        let to_lang = c_str_to_string(to_lang).unwrap_or_else(|| "en".to_string());
        let server_addr =
            c_str_to_string(server_addr).unwrap_or_else(|| "http://127.0.0.1:8000".to_string());
        let api_key = c_str_to_string(api_key).unwrap_or_default();

        let config = TranslatorConfig::new(from_lang, to_lang)
            .with_backend(TranslateBackendKind::LibreTranslate)
            .with_libretranslate_server_addr(server_addr)
            .with_libretranslate_api_key(api_key);
        let translator = Translator::new(config);
        *translator_state().lock() = Some(translator);
        debug!("Translator initialized via FFI (remote LibreTranslate server)");
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
