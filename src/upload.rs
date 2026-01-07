#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use rand::RngCore;
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::io;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::tempdir;

use crate::config::GistFormat;
use crate::gist::render_gist_markdown;

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

#[derive(Debug, Clone)]
pub struct GistUploadResult {
    pub id: String,
    pub raw_url: String,
    pub html_url: String,
}

/// Generate a random delete token (64 hex chars)
fn generate_delete_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn far_future_expires_at() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.saturating_add(60 * 60 * 24 * 365 * 100)
}

pub fn upload_gist(
    upload_url: &str,
    payload_json: &str,
    description: &str,
    format: GistFormat,
) -> Result<UploadResult> {
    ensure_gh_ready()?;

    let (filename, content) = match format {
        GistFormat::Markdown => {
            let md = render_gist_markdown(payload_json)?;
            ("transcript.md".to_string(), md)
        }
        GistFormat::Json => ("agentexport.json".to_string(), payload_json.to_string()),
    };

    let body = serde_json::json!({
        "public": false,
        "description": description,
        "files": {
            filename: {
                "content": content
            }
        }
    });

    let temp = tempdir().context("Failed to create temp dir for gist payload")?;
    let body_path = temp.path().join("gist.json");
    let body_bytes = serde_json::to_vec(&body).context("Failed to serialize gist payload")?;
    fs::write(&body_path, body_bytes).context("Failed to write gist payload")?;

    let output = Command::new("gh")
        .args(["api", "gists", "--input"])
        .arg(&body_path)
        .output()
        .context("Failed to run gh api for gist create")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh api gist create failed: {}", stderr.trim());
    }

    let response: Value =
        serde_json::from_slice(&output.stdout).context("Failed to parse gist response")?;
    let id = response
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing id in gist response")?;

    // Return agentexports.com URL that will proxy and render the gist
    let share_url = format!("https://agentexports.com/g/{}", id);

    Ok(UploadResult {
        id: id.to_string(),
        key: String::new(),
        delete_token: String::new(),
        share_url,
        upload_url: upload_url.to_string(),
        expires_at: far_future_expires_at(),
    })
}

pub fn upload_gist_raw_json(payload_json: &str, description: &str) -> Result<GistUploadResult> {
    ensure_gh_ready()?;

    let filename = "agentexport-map.json";
    let body = serde_json::json!({
        "public": false,
        "description": description,
        "files": {
            filename: {
                "content": payload_json
            }
        }
    });

    let temp = tempdir().context("Failed to create temp dir for gist payload")?;
    let body_path = temp.path().join("gist.json");
    let body_bytes = serde_json::to_vec(&body).context("Failed to serialize gist payload")?;
    fs::write(&body_path, body_bytes).context("Failed to write gist payload")?;

    let output = Command::new("gh")
        .args(["api", "gists", "--input"])
        .arg(&body_path)
        .output()
        .context("Failed to run gh api for gist create")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh api gist create failed: {}", stderr.trim());
    }

    let response: Value =
        serde_json::from_slice(&output.stdout).context("Failed to parse gist response")?;
    let id = response
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing id in gist response")?;
    let html_url = response
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let raw_url = response
        .get("files")
        .and_then(|v| v.get(filename))
        .and_then(|v| v.get("raw_url"))
        .and_then(|v| v.as_str())
        .context("Missing raw_url in gist response")?;

    Ok(GistUploadResult {
        id: id.to_string(),
        raw_url: raw_url.to_string(),
        html_url,
    })
}

fn ensure_gh_ready() -> Result<()> {
    let output = Command::new("gh")
        .args(["auth", "status", "-h", "github.com"])
        .output();

    match output {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let detail = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else {
                stdout.trim().to_string()
            };
            if detail.is_empty() {
                bail!("gh auth status failed; run `gh auth login`");
            }
            bail!("gh auth status failed; run `gh auth login`. {}", detail);
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            bail!("gh not found; install GitHub CLI and run `gh auth login`");
        }
        Err(err) => Err(err.into()),
    }
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
