#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
struct UploadResponse {
    id: String,
    expires_at: u64,
}

/// Result of uploading a blob
#[derive(Debug, Clone)]
pub struct UploadResult {
    pub id: String,
    pub key: String,
    pub key_hash: String,
    pub share_url: String,
    pub upload_url: String,
    pub expires_at: u64,
}

/// Compute SHA256 hash of base64url-encoded key
pub fn compute_key_hash(key_b64: &str) -> String {
    let key_bytes = URL_SAFE_NO_PAD.decode(key_b64).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&key_bytes);
    hex::encode(hasher.finalize())
}

/// Upload encrypted blob to worker, return upload result with all metadata
pub fn upload_blob(upload_url: &str, blob: &[u8], key_b64: &str) -> Result<UploadResult> {
    let endpoint = format!("{}/upload", upload_url.trim_end_matches('/'));
    let key_hash = compute_key_hash(key_b64);

    let response = ureq::post(&endpoint)
        .set("Content-Type", "application/octet-stream")
        .set("X-Key-Hash", &key_hash)
        .send_bytes(blob)
        .context("Failed to upload blob")?;

    if response.status() >= 400 {
        let status = response.status();
        let body = response.into_string().unwrap_or_default();
        bail!("Upload failed: {} - {}", status, body);
    }

    let upload_response: UploadResponse = response
        .into_json()
        .context("Failed to parse upload response")?;

    // Construct final URL with key in fragment
    let base_url = upload_url.trim_end_matches('/');
    let share_url = format!("{}/v/{}#{}", base_url, upload_response.id, key_b64);

    Ok(UploadResult {
        id: upload_response.id,
        key: key_b64.to_string(),
        key_hash,
        share_url,
        upload_url: base_url.to_string(),
        expires_at: upload_response.expires_at,
    })
}

/// Delete a blob from the server
pub fn delete_blob(upload_url: &str, id: &str, key_b64: &str) -> Result<()> {
    let endpoint = format!("{}/blob/{}", upload_url.trim_end_matches('/'), id);
    let key_hash = compute_key_hash(key_b64);

    let response = ureq::delete(&endpoint)
        .set("X-Key-Hash", &key_hash)
        .call()
        .context("Failed to delete blob")?;

    if response.status() >= 400 {
        let status = response.status();
        let body = response.into_string().unwrap_or_default();
        bail!("Delete failed: {} - {}", status, body);
    }

    Ok(())
}

/// Check if a blob exists and is not expired
pub fn check_blob_status(upload_url: &str, id: &str) -> Result<BlobStatus> {
    let endpoint = format!("{}/blob/{}", upload_url.trim_end_matches('/'), id);

    match ureq::head(&endpoint).call() {
        Ok(response) => {
            if response.status() == 200 {
                Ok(BlobStatus::Active)
            } else {
                Ok(BlobStatus::Unknown)
            }
        }
        Err(ureq::Error::Status(404, _)) => Ok(BlobStatus::NotFound),
        Err(ureq::Error::Status(410, _)) => Ok(BlobStatus::Expired),
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobStatus {
    Active,
    Expired,
    NotFound,
    Unknown,
}

#[cfg(test)]
mod tests {
    // Integration tests would require a running worker
    // Unit tests for URL construction

    #[test]
    fn test_url_construction() {
        let base = "https://agentexports.com";
        let id = "abc123def456";
        let key = "SGVsbG8gV29ybGQ";

        let url = format!("{}/v/{}#{}", base.trim_end_matches('/'), id, key);
        assert_eq!(url, "https://agentexports.com/v/abc123def456#SGVsbG8gV29ybGQ");
    }

    #[test]
    fn test_url_with_trailing_slash() {
        let base = "https://agentexports.com/";
        let id = "abc123def456";
        let key = "SGVsbG8gV29ybGQ";

        let url = format!("{}/v/{}#{}", base.trim_end_matches('/'), id, key);
        assert_eq!(url, "https://agentexports.com/v/abc123def456#SGVsbG8gV29ybGQ");
    }
}
