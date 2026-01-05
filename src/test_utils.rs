//! Shared test utilities.
//!
//! All tests that manipulate environment variables or the current directory
//! must use the shared `env_lock()` to prevent race conditions.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Global lock for tests that modify environment variables or current directory.
/// All such tests MUST hold this lock to prevent race conditions.
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// RAII guard for temporarily setting an environment variable.
pub struct EnvGuard {
    key: String,
    old: Option<String>,
}

impl EnvGuard {
    pub fn set(key: &str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self {
            key: key.to_string(),
            old,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(val) = &self.old {
            unsafe {
                std::env::set_var(&self.key, val);
            }
        } else {
            unsafe {
                std::env::remove_var(&self.key);
            }
        }
    }
}

/// RAII guard for temporarily changing the current directory.
pub struct DirGuard {
    original: PathBuf,
}

impl DirGuard {
    pub fn set(path: &Path) -> anyhow::Result<Self> {
        let original = std::env::current_dir()?;
        std::env::set_current_dir(path)?;
        Ok(Self { original })
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}
