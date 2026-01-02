use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use mpv_stt_common::{MpvSttError, Result};
use sha2::{Digest, Sha256};

const NONCE_SIZE: usize = 12;

#[derive(Clone)]
pub struct EncryptionKey {
    cipher: Aes256Gcm,
}

impl EncryptionKey {
    pub fn from_passphrase(passphrase: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(passphrase.as_bytes());
        let key_bytes = hasher.finalize();

        let cipher = Aes256Gcm::new(&key_bytes);
        Self { cipher }
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let nonce_bytes = rand::random::<[u8; NONCE_SIZE]>();
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| MpvSttError::CryptoError(format!("Encryption failed: {}", e)))?;

        let mut result = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);

        Ok(result)
    }

    pub fn decrypt(&self, encrypted: &[u8]) -> Result<Vec<u8>> {
        if encrypted.len() < NONCE_SIZE {
            return Err(MpvSttError::CryptoError(
                "Encrypted data too short".to_string(),
            ));
        }

        let (nonce_bytes, ciphertext) = encrypted.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| MpvSttError::CryptoError(format!("Decryption failed: {}", e)))?;

        Ok(plaintext)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AuthToken([u8; 32]);

impl AuthToken {
    pub fn from_secret(secret: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(secret.as_bytes());
        let hash = hasher.finalize();

        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&hash);
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AuthToken(***)")
    }
}
