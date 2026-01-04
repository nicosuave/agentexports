use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StorageType {
    Agentexport,
    Gist,
}

impl StorageType {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_lowercase().as_str() {
            "agentexport" => Ok(Self::Agentexport),
            "gist" => Ok(Self::Gist),
            _ => bail!("invalid storage_type: must be agentexport or gist"),
        }
    }
}

impl Default for StorageType {
    fn default() -> Self {
        StorageType::Agentexport
    }
}

impl std::fmt::Display for StorageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            StorageType::Agentexport => "agentexport",
            StorageType::Gist => "gist",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Default TTL in days (30, 60, 90, 180, 365, or 0 for forever)
    #[serde(default = "default_ttl")]
    pub default_ttl: u64,

    /// Storage backend (agentexport or gist)
    #[serde(default = "default_storage_type")]
    pub storage_type: StorageType,

    /// Upload URL (default: https://agentexports.com)
    #[serde(default = "default_upload_url")]
    pub upload_url: String,
}

fn default_ttl() -> u64 {
    30
}

fn default_upload_url() -> String {
    "https://agentexports.com".to_string()
}

fn default_storage_type() -> StorageType {
    StorageType::Agentexport
}

fn config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".agentexport").join("config.toml"))
}

impl Config {
    /// Load config from ~/.agentexport/config.toml, returning defaults if file doesn't exist
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(config)
    }

    /// Save config to ~/.agentexport/config.toml
    pub fn save(&self) -> Result<PathBuf> {
        let path = config_path()?;
        let content = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_ttl: default_ttl(),
            storage_type: default_storage_type(),
            upload_url: default_upload_url(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn config_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".agentexport");

        let config = Config {
            default_ttl: 90,
            storage_type: StorageType::Gist,
            upload_url: "https://example.com".to_string(),
        };

        let content = toml::to_string_pretty(&config).unwrap();
        fs::write(&path, &content).unwrap();

        let loaded: Config = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.default_ttl, 90);
        assert_eq!(loaded.storage_type, StorageType::Gist);
        assert_eq!(loaded.upload_url, "https://example.com");
    }

    #[test]
    fn config_defaults() {
        let config = Config::default();
        assert_eq!(config.default_ttl, 30);
        assert_eq!(config.storage_type, StorageType::Agentexport);
        assert_eq!(config.upload_url, "https://agentexports.com");
    }

    #[test]
    fn config_partial_parse() {
        let content = "default_ttl = 60\n";
        let config: Config = toml::from_str(content).unwrap();
        assert_eq!(config.default_ttl, 60);
        assert_eq!(config.storage_type, StorageType::Agentexport);
        assert_eq!(config.upload_url, "https://agentexports.com");
    }

    #[test]
    fn config_storage_type_parse() {
        let content = "storage_type = \"gist\"\n";
        let config: Config = toml::from_str(content).unwrap();
        assert_eq!(config.storage_type, StorageType::Gist);
    }
}
