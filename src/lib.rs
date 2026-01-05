//! agentexport: CLI tool for sharing Claude Code and Codex transcripts.
//!
//! This is the public API for the agentexport library.

pub mod config;
mod crypto;
mod gist;
mod publish;
mod setup;
pub mod shares;
mod terminal;
#[cfg(test)]
pub mod test_utils;
mod transcript;
mod upload;

// Re-export public types from config
pub use config::{Config, GistFormat, StorageType};

// Re-export public types from transcript
pub use transcript::Tool;

// Re-export public types and functions from publish
pub use publish::{
    ClaudeState, PublishOptions, PublishResult, claude_state_path, handle_claude_sessionstart,
    publish, read_claude_state, write_claude_state,
};

// Re-export setup
pub use setup::run as run_setup;

// Re-export transcript utilities needed by external code
pub use transcript::{cache_dir, codex_home_dir, codex_sessions_dir};
