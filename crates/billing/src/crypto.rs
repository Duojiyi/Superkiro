//! Provider key AES-256-GCM encryption and Key Encryption Key (KEK) lifecycle.
//!
//! Spec §7 (Security: Provider Key Encryption, External Master KEK Injection & Rotation).

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use std::fmt;
use std::path::Path;
use thiserror::Error;

pub const KEK_LEN: usize = 32; // 256 bits

#[derive(Error, Debug)]
pub enum CryptoError {
    #[error("Invalid KEK length: expected {expected} bytes, got {actual} bytes")]
    InvalidKekLength { expected: usize, actual: usize },

    #[error("Invalid hex encoding: {0}")]
    HexDecodeError(String),

    #[error("Missing environment variable: {0}")]
    EnvVarNotFound(String),

    #[error("IO error reading KEK file: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Encryption failed")]
    EncryptionError,

    #[error("Decryption failed: corrupted ciphertext or invalid KEK / authentication tag")]
    DecryptionError,

    #[error("Invalid ciphertext format: expected 'v1:<hex_nonce>:<hex_ciphertext>'")]
    InvalidCiphertextFormat,
}

/// 256-bit Key Encryption Key (KEK) for wrapping provider API keys.
///
/// Kept strictly in memory; zeroizes on debug formatting (Spec §7).
#[derive(Clone)]
pub struct MasterKek([u8; KEK_LEN]);

impl fmt::Debug for MasterKek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MasterKek([REDACTED])")
    }
}

impl MasterKek {
    /// Create KEK from 32 raw bytes.
    pub fn from_bytes(bytes: [u8; KEK_LEN]) -> Self {
        Self(bytes)
    }

    /// Parse KEK from a 64-character hex string.
    pub fn from_hex(hex_str: &str) -> Result<Self, CryptoError> {
        let trimmed = hex_str.trim();
        let bytes = hex::decode(trimmed).map_err(|e| CryptoError::HexDecodeError(e.to_string()))?;
        if bytes.len() != KEK_LEN {
            return Err(CryptoError::InvalidKekLength {
                expected: KEK_LEN,
                actual: bytes.len(),
            });
        }
        let mut key = [0u8; KEK_LEN];
        key.copy_from_slice(&bytes);
        Ok(Self(key))
    }

    /// Read KEK from an environment variable (default: `KIRO_MASTER_KEK`).
    pub fn from_env(var_name: &str) -> Result<Self, CryptoError> {
        let val = std::env::var(var_name)
            .map_err(|_| CryptoError::EnvVarNotFound(var_name.to_string()))?;
        Self::from_hex(&val)
    }

    /// Read KEK from a secure external file (Spec §7: 0600 file injection).
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, CryptoError> {
        let content = std::fs::read_to_string(path)?;
        Self::from_hex(&content)
    }

    /// Generate a fresh, cryptographically secure 256-bit KEK.
    pub fn generate_random() -> Result<Self, CryptoError> {
        let rng = SystemRandom::new();
        let mut key = [0u8; KEK_LEN];
        rng.fill(&mut key)
            .map_err(|_| CryptoError::EncryptionError)?;
        Ok(Self(key))
    }

    /// Encrypt a provider API key plaintext into an authenticated ciphertext string.
    /// Format: `v1:<hex_nonce>:<hex_ciphertext_and_tag>`
    pub fn encrypt(&self, plaintext: &str) -> Result<String, CryptoError> {
        let rng = SystemRandom::new();
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rng.fill(&mut nonce_bytes)
            .map_err(|_| CryptoError::EncryptionError)?;

        let unbound_key =
            UnboundKey::new(&AES_256_GCM, &self.0).map_err(|_| CryptoError::EncryptionError)?;
        let key = LessSafeKey::new(unbound_key);

        let nonce = Nonce::try_assume_unique_for_key(&nonce_bytes)
            .map_err(|_| CryptoError::EncryptionError)?;

        let mut in_out = plaintext.as_bytes().to_vec();
        key.seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
            .map_err(|_| CryptoError::EncryptionError)?;

        let hex_nonce = hex::encode(nonce_bytes);
        let hex_payload = hex::encode(in_out);

        Ok(format!("v1:{hex_nonce}:{hex_payload}"))
    }

    /// Seals bytes for one purpose, named by `aad`: the nonce, then the ciphertext and its
    /// tag. What is sealed for one purpose does not open for another.
    pub fn seal_bytes(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| CryptoError::EncryptionError)?;
        let key = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, &self.0).map_err(|_| CryptoError::EncryptionError)?,
        );
        let mut payload = plaintext.to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::from(aad),
            &mut payload,
        )
        .map_err(|_| CryptoError::EncryptionError)?;
        let mut sealed = nonce_bytes.to_vec();
        sealed.extend_from_slice(&payload);
        Ok(sealed)
    }

    /// Opens what [`MasterKek::seal_bytes`] sealed for the same purpose.
    pub fn open_bytes(&self, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if sealed.len() < NONCE_LEN {
            return Err(CryptoError::InvalidCiphertextFormat);
        }
        let (nonce, payload) = sealed.split_at(NONCE_LEN);
        let key = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, &self.0).map_err(|_| CryptoError::DecryptionError)?,
        );
        let nonce =
            Nonce::try_assume_unique_for_key(nonce).map_err(|_| CryptoError::DecryptionError)?;
        let mut payload = payload.to_vec();
        let plaintext = key
            .open_in_place(nonce, Aad::from(aad), &mut payload)
            .map_err(|_| CryptoError::DecryptionError)?;
        Ok(plaintext.to_vec())
    }

    /// Decrypt an authenticated ciphertext string back into the provider API key plaintext.
    pub fn decrypt(&self, ciphertext_str: &str) -> Result<String, CryptoError> {
        let parts: Vec<&str> = ciphertext_str.split(':').collect();
        if parts.len() != 3 || parts[0] != "v1" {
            return Err(CryptoError::InvalidCiphertextFormat);
        }

        let nonce_bytes =
            hex::decode(parts[1]).map_err(|e| CryptoError::HexDecodeError(e.to_string()))?;
        if nonce_bytes.len() != NONCE_LEN {
            return Err(CryptoError::InvalidCiphertextFormat);
        }

        let mut payload =
            hex::decode(parts[2]).map_err(|e| CryptoError::HexDecodeError(e.to_string()))?;

        let unbound_key =
            UnboundKey::new(&AES_256_GCM, &self.0).map_err(|_| CryptoError::DecryptionError)?;
        let key = LessSafeKey::new(unbound_key);

        let nonce = Nonce::try_assume_unique_for_key(&nonce_bytes)
            .map_err(|_| CryptoError::DecryptionError)?;

        let plaintext_slice = key
            .open_in_place(nonce, Aad::empty(), &mut payload)
            .map_err(|_| CryptoError::DecryptionError)?;

        String::from_utf8(plaintext_slice.to_vec()).map_err(|_| CryptoError::DecryptionError)
    }
}

/// Simple hex encoder/decoder using std/core to avoid unnecessary external crate.
pub(crate) mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn decode(hex: &str) -> Result<Vec<u8>, String> {
        if !hex.len().is_multiple_of(2) {
            return Err("Hex string has odd length".to_string());
        }
        (0..hex.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&hex[i..i + 2], 16)
                    .map_err(|e| format!("Invalid byte '{:?}': {}", &hex[i..i + 2], e))
            })
            .collect()
    }
}

/// Mask sensitive provider API key for admin presentation and safe logging (Spec §7).
///
/// Example:
/// - `sk-ant-api03-abcdef1234567890` -> `sk-ant-...7890`
/// - `short` -> `sh***rt`
pub fn mask_provider_key(key: &str) -> String {
    let len = key.len();
    if len <= 8 {
        return "***".to_string();
    }
    if len <= 16 {
        let prefix: String = key.chars().take(2).collect();
        let suffix: String = key
            .chars()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return format!("{prefix}***{suffix}");
    }
    let prefix: String = key.chars().take(6).collect();
    let suffix: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{prefix}...{suffix}")
}

/// Re-encrypt a provider key from old KEK to new KEK (Spec §7 Key Rotation).
pub fn rotate_provider_key(
    old_kek: &MasterKek,
    new_kek: &MasterKek,
    ciphertext: &str,
) -> Result<String, CryptoError> {
    let plaintext = old_kek.decrypt(ciphertext)?;
    new_kek.encrypt(&plaintext)
}

/// Batch re-encrypt provider key collection during master key rotation.
pub fn rotate_all_provider_keys(
    old_kek: &MasterKek,
    new_kek: &MasterKek,
    keys: &mut [String],
) -> Result<usize, CryptoError> {
    let mut count = 0;
    for cipher in keys.iter_mut() {
        let rotated = rotate_provider_key(old_kek, new_kek, cipher)?;
        *cipher = rotated;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_bytes_open_only_for_their_purpose_and_unaltered() {
        let kek = MasterKek::generate_random().unwrap();
        let sealed = kek
            .seal_bytes(b"purpose/1", b"\x00binary\xffpayload")
            .unwrap();
        assert_eq!(
            kek.open_bytes(b"purpose/1", &sealed).unwrap(),
            b"\x00binary\xffpayload"
        );
        assert!(kek.open_bytes(b"purpose/2", &sealed).is_err());
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(kek.open_bytes(b"purpose/1", &tampered).is_err());
        assert!(kek.open_bytes(b"purpose/1", &sealed[..4]).is_err());
        let other = MasterKek::generate_random().unwrap();
        assert!(other.open_bytes(b"purpose/1", &sealed).is_err());
        // A fresh nonce each time: the same bytes never seal the same way twice.
        assert_ne!(
            sealed,
            kek.seal_bytes(b"purpose/1", b"\x00binary\xffpayload")
                .unwrap()
        );
    }

    #[test]
    fn test_kek_encrypt_decrypt_roundtrip() {
        let kek = MasterKek::generate_random().unwrap();
        let raw_key = "sk-ant-api03-secret-upstream-token-123456";

        let encrypted = kek.encrypt(raw_key).unwrap();
        assert!(encrypted.starts_with("v1:"));
        assert_ne!(encrypted, raw_key);

        let decrypted = kek.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, raw_key);
    }

    #[test]
    fn test_kek_tampered_ciphertext_fails_auth_tag() {
        let kek = MasterKek::generate_random().unwrap();
        let encrypted = kek.encrypt("secret-key").unwrap();

        // Tamper with payload
        let mut chars: Vec<char> = encrypted.chars().collect();
        let last_idx = chars.len() - 1;
        chars[last_idx] = if chars[last_idx] == '0' { '1' } else { '0' };
        let tampered: String = chars.into_iter().collect();

        assert!(matches!(
            kek.decrypt(&tampered),
            Err(CryptoError::DecryptionError)
        ));
    }

    #[test]
    fn test_kek_rotation() {
        let old_kek = MasterKek::generate_random().unwrap();
        let new_kek = MasterKek::generate_random().unwrap();
        let raw_key = "sk-openai-key-live-production-998877";

        let old_encrypted = old_kek.encrypt(raw_key).unwrap();
        let new_encrypted = rotate_provider_key(&old_kek, &new_kek, &old_encrypted).unwrap();

        // Old kek cannot decrypt new cipher
        assert!(old_kek.decrypt(&new_encrypted).is_err());
        // New kek successfully decrypts
        assert_eq!(new_kek.decrypt(&new_encrypted).unwrap(), raw_key);
    }

    #[test]
    fn test_mask_provider_key() {
        assert_eq!(
            mask_provider_key("sk-ant-api03-abcdef1234567890"),
            "sk-ant...7890"
        );
        assert_eq!(mask_provider_key("12345"), "***");
    }
}
