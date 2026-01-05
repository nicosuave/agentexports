//! Transcript discovery: finding transcripts by cwd for Claude and Codex.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

use super::types::Tool;

/// Metadata from Codex session_meta event
#[derive(Debug, Clone)]
struct SessionMeta {
    id: String,
    cwd: Option<String>,
    originator: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HistoryEntry {
    session_id: String,
    ts: u64,
}

/// Get the cache directory for agentexport
pub fn cache_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("AGENTEXPORT_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(dir) = std::env::var("TRANSCRIPTCTL_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(dir) = std::env::var("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".cache"))
}

/// Get the Codex sessions directory
pub fn codex_sessions_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("AGENTEXPORT_CODEX_SESSIONS_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(dir) = std::env::var("TRANSCRIPTCTL_CODEX_SESSIONS_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".codex").join("sessions"))
}

/// Get the Codex home directory
pub fn codex_home_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CODEX_HOME") {
        if !dir.trim().is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".codex"))
}

fn claude_projects_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".claude").join("projects"))
}

/// Encode a cwd path to Claude's project folder name format.
/// Rules: /. -> /- (hidden dirs), / -> -, _ -> -
pub fn cwd_to_project_folder(cwd: &str) -> String {
    cwd.replace("/.", "/-").replace(['/', '_'], "-")
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn is_fresh(modified: SystemTime, max_age_minutes: u64) -> bool {
    if max_age_minutes == 0 {
        return true;
    }
    let max_age = Duration::from_secs(max_age_minutes.saturating_mul(60));
    match SystemTime::now().duration_since(modified) {
        Ok(age) => age <= max_age,
        Err(_) => true,
    }
}

/// Read session_id from the first few lines of a transcript
fn read_session_id_from_transcript(path: &Path) -> Result<Option<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(20) {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(id) = value.get("sessionId").and_then(|v| v.as_str()) {
            return Ok(Some(id.to_string()));
        }
    }
    Ok(None)
}

/// Find the most recent Claude transcript for a given cwd.
/// Returns (transcript_path, session_id) if found.
fn find_claude_transcript_for_cwd(
    cwd: &str,
    max_age_minutes: u64,
) -> Result<Option<(PathBuf, String)>> {
    let projects_dir = claude_projects_dir()?;
    let folder_name = cwd_to_project_folder(cwd);
    let project_dir = projects_dir.join(&folder_name);

    if !project_dir.exists() {
        return Ok(None);
    }

    // Find the most recently modified .jsonl file
    let mut best: Option<(PathBuf, SystemTime)> = None;
    for entry in fs::read_dir(&project_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let meta = entry.metadata()?;
        if !meta.is_file() || meta.len() == 0 {
            continue;
        }
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        if max_age_minutes > 0 && !is_fresh(modified, max_age_minutes) {
            continue;
        }
        let dominated = match best.as_ref() {
            Some((_, best_time)) => modified <= *best_time,
            None => false,
        };
        if !dominated {
            best = Some((path, modified));
        }
    }

    let Some((path, _)) = best else {
        return Ok(None);
    };

    // Extract session_id from filename (format: {session_id}.jsonl or agent-{id}.jsonl)
    let filename = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let session_id = if filename.starts_with("agent-") {
        // Agent files use a different ID scheme, read from content
        read_session_id_from_transcript(&path)?
    } else {
        // Regular session files use UUID as filename
        Some(filename.to_string())
    };

    match session_id {
        Some(id) => Ok(Some((path, id))),
        None => Ok(None),
    }
}

fn read_session_meta(path: &Path) -> Result<Option<SessionMeta>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(50) {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value.get("type").and_then(|v| v.as_str()) != Some("session_meta") {
            continue;
        }
        let payload = value.get("payload");
        let id = payload
            .and_then(|p| p.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            return Ok(None);
        }
        let cwd = payload
            .and_then(|p| p.get("cwd"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let originator = payload
            .and_then(|p| p.get("originator"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        return Ok(Some(SessionMeta {
            id,
            cwd,
            originator,
        }));
    }
    Ok(None)
}

fn is_interactive_originator(originator: Option<&str>) -> bool {
    match originator {
        Some("codex_exec") => false,
        Some("codex_cli_rs") => true,
        Some(_) => true,
        None => true,
    }
}

/// Find Codex transcript for a given cwd using history.jsonl
pub fn find_codex_transcript_for_cwd_from_history(
    cwd: &str,
    max_age_minutes: u64,
) -> Result<Option<(PathBuf, String)>> {
    let root = codex_sessions_dir()?;
    if !root.exists() {
        return Ok(None);
    }

    let mut session_map: HashMap<String, (PathBuf, SystemTime)> = HashMap::new();
    for entry in WalkDir::new(&root).follow_links(true) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let meta = entry.metadata()?;
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        if max_age_minutes > 0 && !is_fresh(modified, max_age_minutes) {
            continue;
        }
        let session_meta = match read_session_meta(path)? {
            Some(session_meta) => session_meta,
            None => continue,
        };
        if session_meta.cwd.as_deref() != Some(cwd) {
            continue;
        }
        if !is_interactive_originator(session_meta.originator.as_deref()) {
            continue;
        }
        let replace = match session_map.get(&session_meta.id) {
            Some((_, existing_modified)) => modified >= *existing_modified,
            None => true,
        };
        if replace {
            session_map.insert(session_meta.id, (path.to_path_buf(), modified));
        }
    }

    if session_map.is_empty() {
        return Ok(None);
    }

    let history_path = codex_home_dir()?.join("history.jsonl");
    if !history_path.exists() {
        return Ok(None);
    }

    let now = now_unix();
    let max_age_seconds = max_age_minutes.saturating_mul(60);
    let file = File::open(&history_path)?;
    let reader = BufReader::new(file);
    let mut best: Option<(u64, String)> = None;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: HistoryEntry = match serde_json::from_str(trimmed) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if max_age_minutes > 0 && now.saturating_sub(entry.ts) > max_age_seconds {
            continue;
        }
        if session_map.contains_key(&entry.session_id) {
            let replace = match best.as_ref() {
                Some((best_ts, _)) => entry.ts >= *best_ts,
                None => true,
            };
            if replace {
                best = Some((entry.ts, entry.session_id));
            }
        }
    }

    let Some((_, session_id)) = best else {
        return Ok(None);
    };
    let Some((path, _)) = session_map.get(&session_id) else {
        return Ok(None);
    };
    Ok(Some((path.clone(), session_id)))
}

/// Validate that a transcript file exists, is not empty, and is fresh enough
pub fn validate_transcript_fresh(path: &Path, max_age_minutes: u64) -> Result<(u64, u64)> {
    let meta =
        fs::metadata(path).with_context(|| format!("missing transcript: {}", path.display()))?;
    if !meta.is_file() {
        bail!("transcript is not a file: {}", path.display());
    }
    let size = meta.len();
    if size == 0 {
        bail!("transcript is empty: {}", path.display());
    }
    let modified = meta.modified().context("missing mtime")?;
    if !is_fresh(modified, max_age_minutes) {
        bail!("transcript is stale: {}", path.display());
    }
    let modified_at = modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok((size, modified_at))
}

/// Check if file contains a needle in the first max_bytes
pub fn file_contains(path: &Path, needle: &str, max_bytes: usize) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; max_bytes];
    let n = file.read(&mut buf)?;
    let content = String::from_utf8_lossy(&buf[..n]);
    Ok(content.contains(needle))
}

/// Resolve Claude transcript path, either from explicit path or by cwd discovery
pub fn resolve_claude_transcript(
    transcript_arg: Option<PathBuf>,
    max_age_minutes: u64,
) -> Result<(PathBuf, Option<String>)> {
    // If explicit transcript path provided, use it
    if let Some(path) = transcript_arg {
        let session_id = read_session_id_from_transcript(&path)?.or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| !s.starts_with("agent-"))
                .map(|s| s.to_string())
        });
        return Ok((path, session_id));
    }

    // Primary method: find transcript by cwd (no hook needed)
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(|s| s.to_string()))
        .context("unable to resolve cwd; pass --transcript")?;

    if let Some((path, session_id)) = find_claude_transcript_for_cwd(&cwd, max_age_minutes)? {
        return Ok((path, Some(session_id)));
    }

    bail!(
        "no recent Claude transcript found for current directory; run from the Claude session directory, or pass --transcript"
    )
}

/// Resolve Codex transcript path, either from explicit path or by history discovery
pub fn resolve_codex_transcript(
    transcript_arg: Option<PathBuf>,
    max_age_minutes: u64,
) -> Result<(PathBuf, Option<String>)> {
    if let Some(path) = transcript_arg {
        return Ok((path, None));
    }

    let cwd = std::env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(|s| s.to_string()))
        .context("unable to resolve cwd; pass --transcript")?;

    if let Some((path, thread_id)) =
        find_codex_transcript_for_cwd_from_history(&cwd, max_age_minutes)?
    {
        return Ok((path, Some(thread_id)));
    }

    bail!(
        "unable to resolve codex transcript from history; ensure history is enabled and run from the Codex session cwd, or pass --transcript"
    );
}

/// Resolve transcript based on tool type
pub fn resolve_transcript(
    tool: Tool,
    transcript_arg: Option<PathBuf>,
    max_age_minutes: u64,
) -> Result<(PathBuf, Option<String>, Option<String>)> {
    match tool {
        Tool::Claude => {
            let (path, session_id) = resolve_claude_transcript(transcript_arg, max_age_minutes)?;
            Ok((path, session_id, None))
        }
        Tool::Codex => {
            let (path, thread_id) = resolve_codex_transcript(transcript_arg, max_age_minutes)?;
            Ok((path, None, thread_id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{env_lock, DirGuard, EnvGuard};
    use tempfile::TempDir;

    #[test]
    fn cache_dir_respects_env_override() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _guard = EnvGuard::set("AGENTEXPORT_CACHE_DIR", tmp.path().to_str().unwrap());
        let dir = cache_dir().unwrap();
        assert_eq!(dir, tmp.path());
    }

    #[test]
    fn cwd_to_project_folder_encoding() {
        assert_eq!(
            cwd_to_project_folder("/Users/nico/Code/foo"),
            "-Users-nico-Code-foo"
        );
        assert_eq!(
            cwd_to_project_folder("/Users/nico/.claude/hooks"),
            "-Users-nico--claude-hooks"
        );
        assert_eq!(
            cwd_to_project_folder("/Users/nico/Code/uv_run"),
            "-Users-nico-Code-uv-run"
        );
    }

    #[test]
    fn find_codex_transcript_for_cwd_from_history_prefers_latest_session() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _guard_sessions = EnvGuard::set(
            "AGENTEXPORT_CODEX_SESSIONS_DIR",
            tmp.path().to_str().unwrap(),
        );
        let _guard_home = EnvGuard::set("CODEX_HOME", tmp.path().to_str().unwrap());

        let first = tmp.path().join("first.jsonl");
        fs::write(
            &first,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"sess-a\",\"cwd\":\"/work\",\"originator\":\"codex_cli_rs\"}}\n",
        )
        .unwrap();
        let second = tmp.path().join("second.jsonl");
        fs::write(
            &second,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"sess-b\",\"cwd\":\"/work\",\"originator\":\"codex_cli_rs\"}}\n",
        )
        .unwrap();

        let history_path = tmp.path().join("history.jsonl");
        fs::write(
            &history_path,
            "{\"session_id\":\"sess-a\",\"ts\":1,\"text\":\"old\"}\n{\"session_id\":\"sess-b\",\"ts\":2,\"text\":\"new\"}\n",
        )
        .unwrap();

        let found = find_codex_transcript_for_cwd_from_history("/work", 0)
            .unwrap()
            .unwrap();
        assert_eq!(found.0, second);
        assert_eq!(found.1, "sess-b");
    }

    #[test]
    fn resolve_claude_finds_transcript_by_cwd() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _guard = EnvGuard::set("AGENTEXPORT_CACHE_DIR", tmp.path().to_str().unwrap());

        // Create a fake cwd and its corresponding project folder
        let cwd = tmp.path().join("work");
        fs::create_dir_all(&cwd).unwrap();
        let cwd = fs::canonicalize(&cwd).unwrap();

        // Create the .claude/projects dir structure that Claude uses
        let folder_name = cwd_to_project_folder(cwd.to_str().unwrap());
        let project_dir = tmp
            .path()
            .join(".claude")
            .join("projects")
            .join(&folder_name);
        fs::create_dir_all(&project_dir).unwrap();

        // Override HOME to use our temp dir for .claude/projects
        let _guard_home = EnvGuard::set("HOME", tmp.path().to_str().unwrap());

        // Create a transcript file with proper session ID in filename
        let transcript = project_dir.join("sess-abc.jsonl");
        fs::write(
            &transcript,
            "{\"sessionId\":\"sess-abc\",\"type\":\"user\",\"message\":{\"content\":\"Hello\"}}\n",
        )
        .unwrap();

        let _dir_guard = DirGuard::set(&cwd).unwrap();

        let (path, session_id) = resolve_claude_transcript(None, 0).unwrap();
        assert_eq!(session_id.as_deref(), Some("sess-abc"));
        assert_eq!(path, transcript);
    }

    #[test]
    fn resolve_codex_uses_history_for_current_cwd() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        let cwd = tmp.path().join("work");
        fs::create_dir_all(&cwd).unwrap();
        let cwd = fs::canonicalize(&cwd).unwrap();

        let _guard_sessions = EnvGuard::set(
            "AGENTEXPORT_CODEX_SESSIONS_DIR",
            sessions_dir.to_str().unwrap(),
        );
        let _guard_home = EnvGuard::set("CODEX_HOME", tmp.path().to_str().unwrap());
        let _dir_guard = DirGuard::set(&cwd).unwrap();

        let session_id = "sess-1";
        let session_path = sessions_dir.join("rollout-sess-1.jsonl");
        fs::write(
            &session_path,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{session_id}\",\"cwd\":\"{}\",\"originator\":\"codex_cli_rs\"}}}}\n",
                cwd.display()
            ),
        )
        .unwrap();

        let history_path = tmp.path().join("history.jsonl");
        fs::write(
            &history_path,
            format!("{{\"session_id\":\"{session_id}\",\"ts\":1,\"text\":\"hello\"}}\n"),
        )
        .unwrap();

        let (path, thread_id) = resolve_codex_transcript(None, 0).unwrap();
        assert_eq!(thread_id.as_deref(), Some(session_id));
        assert_eq!(path, session_path);
    }

    #[test]
    fn resolve_codex_fails_without_history() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        let cwd = tmp.path().join("work");
        fs::create_dir_all(&cwd).unwrap();
        let cwd = fs::canonicalize(&cwd).unwrap();

        let _guard_sessions = EnvGuard::set(
            "AGENTEXPORT_CODEX_SESSIONS_DIR",
            sessions_dir.to_str().unwrap(),
        );
        let _guard_home = EnvGuard::set("CODEX_HOME", tmp.path().to_str().unwrap());
        let _dir_guard = DirGuard::set(&cwd).unwrap();

        let session_path = sessions_dir.join("rollout-sess-1.jsonl");
        fs::write(
            &session_path,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"sess-1\",\"cwd\":\"{}\",\"originator\":\"codex_cli_rs\"}}}}\n",
                cwd.display()
            ),
        )
        .unwrap();

        let err = resolve_codex_transcript(None, 0).unwrap_err();
        assert!(err
            .to_string()
            .contains("unable to resolve codex transcript from history"));
    }
}
