use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use flate2::{write::GzEncoder, Compression};
use rand::RngCore;
use std::io::Write;

/// Result of encrypting content
pub struct EncryptionResult {
    /// IV (12 bytes) || ciphertext (includes auth tag)
    pub blob: Vec<u8>,
    /// 32-byte key, base64url encoded for URL fragment
    pub key_b64: String,
}

/// Compress and encrypt HTML content with AES-256-GCM
/// Returns blob (IV + ciphertext) and base64url-encoded key
pub fn encrypt_html(html: &str) -> Result<EncryptionResult> {
    // Compress with gzip
    let compressed = gzip_compress(html.as_bytes())?;

    // Generate random 256-bit key
    let mut key_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key_bytes);

    // Generate random 96-bit IV/nonce
    let mut iv_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut iv_bytes);
    let nonce = Nonce::from_slice(&iv_bytes);

    // Create cipher and encrypt
    let cipher =
        Aes256Gcm::new_from_slice(&key_bytes).context("Failed to create cipher")?;

    let ciphertext = cipher
        .encrypt(nonce, compressed.as_slice())
        .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

    // Combine IV + ciphertext
    let mut blob = Vec::with_capacity(12 + ciphertext.len());
    blob.extend_from_slice(&iv_bytes);
    blob.extend_from_slice(&ciphertext);

    // Encode key as base64url (no padding)
    let key_b64 = URL_SAFE_NO_PAD.encode(key_bytes);

    Ok(EncryptionResult { blob, key_b64 })
}

fn gzip_compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    let compressed = encoder.finish()?;
    Ok(compressed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;

    #[test]
    fn test_encrypt_produces_valid_blob() {
        let html = "<html><body>Hello, World!</body></html>";
        let result = encrypt_html(html).unwrap();

        // Verify blob structure: 12 bytes IV + ciphertext
        assert!(result.blob.len() > 12);

        // Verify key encoding
        let key_bytes = URL_SAFE_NO_PAD.decode(&result.key_b64).unwrap();
        assert_eq!(key_bytes.len(), 32);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let html = "<html><body>Hello, World!</body></html>";
        let result = encrypt_html(html).unwrap();

        // Decode key
        let key_bytes = URL_SAFE_NO_PAD.decode(&result.key_b64).unwrap();

        // Extract IV and ciphertext
        let iv = &result.blob[..12];
        let ciphertext = &result.blob[12..];

        // Decrypt
        let cipher = Aes256Gcm::new_from_slice(&key_bytes).unwrap();
        let nonce = Nonce::from_slice(iv);
        let compressed = cipher.decrypt(nonce, ciphertext).unwrap();

        // Decompress
        let mut decoder = GzDecoder::new(&compressed[..]);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed).unwrap();

        assert_eq!(decompressed, html);
    }

    #[test]
    fn test_compression_reduces_size() {
        // Repetitive content compresses well
        let html = "<html><body>".to_string() + &"Hello ".repeat(1000) + "</body></html>";
        let result = encrypt_html(&html).unwrap();

        // Blob should be smaller than original (minus some overhead)
        assert!(result.blob.len() < html.len());
    }
}
