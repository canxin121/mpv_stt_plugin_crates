use mpv_stt_common::{MpvSttError, Result};
use mpv_stt_crypto::EncryptionKey;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CompressionFormat {
    Opus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    AudioChunk {
        request_id: u64,
        chunk_index: u32,
        total_chunks: u32,
        duration_ms: u64,
        data: Vec<u8>,
        auth_token: [u8; 32],
        compression: CompressionFormat,
    },
    Cancel {
        request_id: u64,
        auth_token: [u8; 32],
    },
    Result {
        request_id: u64,
        chunk_index: u32,
        total_chunks: u32,
        data: Vec<u8>,
    },
    Error {
        request_id: u64,
        message: String,
    },
}

impl Message {
    pub fn request_id(&self) -> u64 {
        match self {
            Message::AudioChunk { request_id, .. }
            | Message::Cancel { request_id, .. }
            | Message::Result { request_id, .. }
            | Message::Error { request_id, .. } => *request_id,
        }
    }

    pub fn auth_token(&self) -> Option<&[u8; 32]> {
        match self {
            Message::AudioChunk { auth_token, .. } | Message::Cancel { auth_token, .. } => {
                Some(auth_token)
            }
            _ => None,
        }
    }

    pub fn encode(&self, encryption_key: Option<&EncryptionKey>) -> Result<Vec<u8>> {
        let serialized = postcard::to_allocvec(self)
            .map_err(|e| MpvSttError::SttFailed(format!("postcard encode failed: {}", e)))?;

        if let Some(key) = encryption_key {
            key.encrypt(&serialized)
        } else {
            Ok(serialized)
        }
    }

    pub fn decode(data: &[u8], encryption_key: Option<&EncryptionKey>) -> Result<Self> {
        let decrypted = if let Some(key) = encryption_key {
            key.decrypt(data)?
        } else {
            data.to_vec()
        };

        postcard::from_bytes(&decrypted)
            .map_err(|e| MpvSttError::SttFailed(format!("postcard decode failed: {}", e)))
    }
}

#[derive(Debug)]
pub struct TranscriptionJob {
    pub request_id: u64,
    pub audio_data: Vec<u8>,
    pub duration_ms: u64,
    /// Timestamp recorded when the request is accepted by the HTTP handler.
    pub enqueue_at: Instant,
}

#[derive(Debug)]
pub enum JobResult {
    Success {
        request_id: u64,
        srt_data: Vec<u8>,
        metrics: JobMetrics,
    },
    Error { request_id: u64, message: String },
}

#[derive(Debug, Clone, Copy)]
pub struct JobMetrics {
    /// Time from enqueue to worker picking up the job.
    pub queue_wait_ms: u64,
    /// Time spent inside the STT runner (rough proxy for inference).
    pub inference_ms: u64,
    /// End-to-end time inside worker thread (queue wait + inference + post).
    pub worker_total_ms: u64,
}
