use thiserror::Error;

#[derive(Error, Debug)]
pub enum MpvSttError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Process execution failed: {0}")]
    ProcessFailed(String),

    #[error("Process timed out: {0}")]
    ProcessTimeout(String),

    #[error("Invalid SRT format: {0}")]
    InvalidSrt(String),

    #[error("Translation failed: {0}")]
    TranslationFailed(String),

    #[error("Audio extraction failed: {0}")]
    AudioExtractionFailed(String),

    #[error("Audio extraction cancelled")]
    AudioExtractionCancelled,

    #[error("WAV error: {0}")]
    Wav(String),

    #[error("STT execution failed: {0}")]
    SttFailed(String),

    #[error("STT execution cancelled")]
    SttCancelled,

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Encryption/Decryption error: {0}")]
    CryptoError(String),
}

pub type Result<T> = std::result::Result<T, MpvSttError>;

impl From<hound::Error> for MpvSttError {
    fn from(err: hound::Error) -> Self {
        MpvSttError::Wav(err.to_string())
    }
}
