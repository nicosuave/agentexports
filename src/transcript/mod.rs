//! Transcript handling: discovery, parsing, and types.

mod discovery;
mod parser;
mod types;

pub use discovery::{
    cache_dir, codex_home_dir, codex_sessions_dir, file_contains, resolve_transcript,
    validate_transcript_fresh,
};
pub use parser::{extract_transcript_meta, parse_transcript};
pub use types::{SharePayload, Tool};

// Re-export for tests
#[cfg(test)]
pub use discovery::cwd_to_project_folder;
