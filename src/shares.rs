//! Local shares storage for managing uploaded transcripts.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use time::OffsetDateTime;

/// A shared transcript record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Share {
    pub id: String,
    pub key: String,
    pub key_hash: String,
    pub upload_url: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub tool: String,
    pub transcript_path: String,
}

impl Share {
    /// Get the full share URL
    pub fn url(&self) -> String {
        format!("{}/v/{}#{}", self.upload_url, self.id, self.key)
    }

    /// Check if this share has expired (based on local time)
    pub fn is_expired(&self) -> bool {
        OffsetDateTime::now_utc() > self.expires_at
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SharesFile {
    shares: Vec<Share>,
}

/// Get the path to the shares file
fn shares_file_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let dir = PathBuf::from(home).join(".agentexport");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("shares.json"))
}

/// Load all shares from local storage
pub fn load_shares() -> Result<Vec<Share>> {
    let path = shares_file_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&path).context("Failed to read shares file")?;
    let file: SharesFile = serde_json::from_str(&content).context("Failed to parse shares file")?;
    Ok(file.shares)
}

/// Save a new share to local storage
pub fn save_share(share: &Share) -> Result<()> {
    let mut shares = load_shares().unwrap_or_default();

    // Check if this share already exists (by id + upload_url)
    let existing = shares
        .iter()
        .position(|s| s.id == share.id && s.upload_url == share.upload_url);

    if let Some(idx) = existing {
        shares[idx] = share.clone();
    } else {
        shares.push(share.clone());
    }

    write_shares(&shares)
}

/// Remove a share from local storage by id
pub fn remove_share(id: &str) -> Result<Option<Share>> {
    let mut shares = load_shares()?;

    let idx = shares.iter().position(|s| s.id == id);
    let removed = idx.map(|i| shares.remove(i));

    if removed.is_some() {
        write_shares(&shares)?;
    }

    Ok(removed)
}

/// Get a share by id
pub fn get_share(id: &str) -> Result<Option<Share>> {
    let shares = load_shares()?;
    Ok(shares.into_iter().find(|s| s.id == id))
}

/// Write shares to disk
fn write_shares(shares: &[Share]) -> Result<()> {
    let path = shares_file_path()?;
    let file = SharesFile {
        shares: shares.to_vec(),
    };
    let content = serde_json::to_string_pretty(&file)?;
    fs::write(&path, format!("{content}\n")).context("Failed to write shares file")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_share(id: &str) -> Share {
        Share {
            id: id.to_string(),
            key: "key123".to_string(),
            key_hash: "hash123".to_string(),
            upload_url: "https://example.com".to_string(),
            created_at: OffsetDateTime::now_utc(),
            expires_at: OffsetDateTime::now_utc(),
            tool: "claude".to_string(),
            transcript_path: "/tmp/test.jsonl".to_string(),
        }
    }

    #[test]
    fn test_share_url() {
        let share = make_test_share("abc123");
        assert_eq!(share.url(), "https://example.com/v/abc123#key123");
    }

    #[test]
    fn test_share_is_expired() {
        let mut share = make_test_share("abc123");

        // Set expires_at to the past
        share.expires_at = OffsetDateTime::now_utc() - time::Duration::hours(1);
        assert!(share.is_expired());

        // Set expires_at to the future
        share.expires_at = OffsetDateTime::now_utc() + time::Duration::hours(1);
        assert!(!share.is_expired());
    }

    #[test]
    fn test_shares_file_serialization() {
        let share = make_test_share("test123");
        let file = SharesFile {
            shares: vec![share.clone()],
        };

        let json = serde_json::to_string(&file).unwrap();
        let parsed: SharesFile = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.shares.len(), 1);
        assert_eq!(parsed.shares[0].id, "test123");
    }
}
