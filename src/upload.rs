use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
struct UploadResponse {
    id: String,
}

/// Upload encrypted blob to worker, return share URL
pub fn upload_blob(upload_url: &str, blob: &[u8], key_b64: &str) -> Result<String> {
    let endpoint = format!("{}/upload", upload_url.trim_end_matches('/'));

    let response = ureq::post(&endpoint)
        .set("Content-Type", "application/octet-stream")
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

    Ok(share_url)
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
