#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use rand::RngCore;
use serde::Deserialize;

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
    pub delete_token: String,
    pub share_url: String,
    pub upload_url: String,
    pub expires_at: u64,
}

/// Generate a random delete token (64 hex chars)
fn generate_delete_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Upload encrypted blob to worker, return upload result with all metadata
pub fn upload_blob(
    upload_url: &str,
    blob: &[u8],
    key_b64: &str,
    ttl_days: u64,
) -> Result<UploadResult> {
    let endpoint = format!("{}/upload", upload_url.trim_end_matches('/'));
    let delete_token = generate_delete_token();

    let response = ureq::post(&endpoint)
        .set("Content-Type", "application/octet-stream")
        .set("X-Delete-Token", &delete_token)
        .set("X-TTL-Days", &ttl_days.to_string())
        .send_bytes(blob)
        .context("Failed to upload blob")?;

    if response.status() >= 400 {
        let status = response.status();
        let body = response.into_string().unwrap_or_default();
        bail!("Upload failed: {status} - {body}");
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
        delete_token,
        share_url,
        upload_url: base_url.to_string(),
        expires_at: upload_response.expires_at,
    })
}

/// Delete a blob from the server using the delete token
pub fn delete_blob(upload_url: &str, id: &str, delete_token: &str) -> Result<()> {
    let endpoint = format!("{}/blob/{}", upload_url.trim_end_matches('/'), id);

    let response = ureq::delete(&endpoint)
        .set("X-Delete-Token", delete_token)
        .call()
        .context("Failed to delete blob")?;

    if response.status() >= 400 {
        let status = response.status();
        let body = response.into_string().unwrap_or_default();
        bail!("Delete failed: {status} - {body}");
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
        assert_eq!(
            url,
            "https://agentexports.com/v/abc123def456#SGVsbG8gV29ybGQ"
        );
    }

    #[test]
    fn test_url_with_trailing_slash() {
        let base = "https://agentexports.com/";
        let id = "abc123def456";
        let key = "SGVsbG8gV29ybGQ";

        let url = format!("{}/v/{}#{}", base.trim_end_matches('/'), id, key);
        assert_eq!(
            url,
            "https://agentexports.com/v/abc123def456#SGVsbG8gV29ybGQ"
        );
    }
}
