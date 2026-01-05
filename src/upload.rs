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

fn far_future_expires_at() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.saturating_add(60 * 60 * 24 * 365 * 100)
}

/// Render payload JSON into a markdown document for GitHub Gist
pub fn render_gist_markdown(payload_json: &str) -> Result<String> {
    let payload: serde_json::Value = serde_json::from_str(payload_json)
        .context("Failed to parse payload JSON")?;

    let mut md = String::new();

    // Title
    let title = payload.get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Agent Export");
    md.push_str(&format!("# {}\n\n", title));

    // Metadata
    let tool = payload.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let model = payload.get("model").and_then(|v| v.as_str());
    let models = payload.get("models").and_then(|v| v.as_array());
    let shared_at = payload.get("shared_at").and_then(|v| v.as_str()).unwrap_or("");

    let model_str = if let Some(m) = model {
        m.to_string()
    } else if let Some(ms) = models {
        ms.iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(" + ")
    } else {
        String::new()
    };

    if !tool.is_empty() || !model_str.is_empty() || !shared_at.is_empty() {
        let mut meta_parts = Vec::new();
        if !tool.is_empty() { meta_parts.push(tool.to_string()); }
        if !model_str.is_empty() { meta_parts.push(model_str); }
        if !shared_at.is_empty() { meta_parts.push(shared_at.to_string()); }
        md.push_str(&format!("*{}*\n\n", meta_parts.join(" · ")));
    }

    // Compaction summary (if present)
    if let Some(summary) = payload.get("compaction_summary").and_then(|v| v.as_str()) {
        md.push_str(&format!("> **Session Summary:** {}\n\n", summary));
    }

    md.push_str("---\n\n");

    // Messages
    if let Some(messages) = payload.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("assistant");
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let msg_model = msg.get("model").and_then(|v| v.as_str());

            // Role header
            let role_display = match role {
                "user" => "User",
                "assistant" => "Assistant",
                "tool" => "Tool",
                "thinking" => "Thinking",
                "system" => "System",
                _ => role,
            };

            let model_suffix = msg_model.map(|m| format!(" ({})", m)).unwrap_or_default();
            md.push_str(&format!("### {}{}\n\n", role_display, model_suffix));

            // Content - for tool messages, wrap in code block if not already
            if role == "tool" && !content.trim().starts_with("```") {
                // Check if it looks like JSON or code
                let trimmed = content.trim();
                if trimmed.starts_with('{') || trimmed.starts_with('[') || trimmed.contains('\n') {
                    md.push_str("```\n");
                    md.push_str(content);
                    if !content.ends_with('\n') { md.push('\n'); }
                    md.push_str("```\n\n");
                } else {
                    md.push_str(&format!("`{}`\n\n", content));
                }
            } else {
                md.push_str(content);
                if !content.ends_with('\n') { md.push('\n'); }
                md.push('\n');
            }

            // Raw/details section (collapsed)
            if let Some(raw) = msg.get("raw").and_then(|v| v.as_str()) {
                let label = msg.get("raw_label").and_then(|v| v.as_str()).unwrap_or("Details");
                md.push_str(&format!("<details>\n<summary>{}</summary>\n\n```json\n{}\n```\n\n</details>\n\n", label, raw));
            }
        }
    }

    // Token stats
    let input_tokens = payload.get("total_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let output_tokens = payload.get("total_output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let cache_read = payload.get("total_cache_read_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let cache_write = payload.get("total_cache_creation_tokens").and_then(|v| v.as_u64()).unwrap_or(0);

    if input_tokens > 0 || output_tokens > 0 {
        md.push_str("---\n\n");
        let mut stats = Vec::new();
        if input_tokens > 0 { stats.push(format!("Input: {} tokens", input_tokens)); }
        if output_tokens > 0 { stats.push(format!("Output: {} tokens", output_tokens)); }
        if cache_read > 0 { stats.push(format!("Cache read: {} tokens", cache_read)); }
        if cache_write > 0 { stats.push(format!("Cache write: {} tokens", cache_write)); }
        md.push_str(&format!("*{}*\n", stats.join(" · ")));
    }

    Ok(md)
}

pub fn upload_gist(upload_url: &str, payload_json: &str, description: &str, format: GistFormat) -> Result<UploadResult> {
    ensure_gh_ready()?;

    let (filename, content) = match format {
        GistFormat::Markdown => {
            let md = render_gist_markdown(payload_json)?;
            ("transcript.md".to_string(), md)
        }
        GistFormat::Json => {
            ("agentexport.json".to_string(), payload_json.to_string())
        }
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
