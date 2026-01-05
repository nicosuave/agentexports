use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use walkdir::WalkDir;

pub mod config;
mod crypto;
mod setup;
pub mod shares;
mod upload;

pub use config::{Config, GistFormat, StorageType};
pub use setup::run as run_setup;

const APP_NAME: &str = "agentexport";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, clap::ValueEnum)]
pub enum Tool {
    Claude,
    Codex,
}

impl Tool {
    pub fn as_str(self) -> &'static str {
        match self {
            Tool::Claude => "claude",
            Tool::Codex => "codex",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClaudeState {
    pub term_key: String,
    pub session_id: String,
    pub transcript_path: String,
    pub cwd: String,
    pub updated_at: u64,
}

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

#[derive(Debug)]
pub struct PublishOptions {
    pub tool: Tool,
    pub term_key: Option<String>,
    pub transcript: Option<PathBuf>,
    pub max_age_minutes: u64,
    pub out: Option<PathBuf>,
    pub dry_run: bool,
    pub upload_url: Option<String>,
    pub render: bool,
    pub ttl_days: u64,
    pub storage_type: StorageType,
    pub gist_format: GistFormat,
    pub title: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PublishResult {
    pub status: String,
    pub tool: String,
    pub term_key: String,
    pub transcript_path: String,
    pub gzip_path: String,
    pub input_bytes: u64,
    pub gzip_bytes: u64,
    pub modified_at: u64,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub render_path: Option<String>,
    pub share_url: Option<String>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderedMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct TranscriptMeta {
    slug: Option<String>,
    first_user_message: Option<String>,
    /// Compaction summary (from Claude's summary events, NOT a title)
    compaction_summary: Option<String>,
}

/// Payload sent to the viewer (encrypted JSON)
#[derive(Debug, Clone, Serialize)]
pub struct SharePayload {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Compaction summary from Claude (displayed in conversation, NOT as title)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction_summary: Option<String>,
    pub shared_at: String,
    /// Primary model (most used), shown in header
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// All models used, for "model1 + model2" display if multiple
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    pub messages: Vec<RenderedMessage>,
    /// Token usage totals (if available)
    #[serde(skip_serializing_if = "is_zero")]
    pub total_input_tokens: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub total_output_tokens: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub total_cache_read_tokens: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub total_cache_creation_tokens: u64,
}

fn is_zero(val: &u64) -> bool {
    *val == 0
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

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

fn codex_home_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CODEX_HOME") {
        if !dir.trim().is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".codex"))
}

fn state_dir(tool: Tool) -> Result<PathBuf> {
    Ok(cache_dir()?.join(APP_NAME).join(tool.as_str()))
}

pub fn claude_state_path(term_key: &str) -> Result<PathBuf> {
    Ok(state_dir(Tool::Claude)?.join(format!("{term_key}.json")))
}

pub fn compute_term_key(
    tty: &str,
    tmux_pane: Option<&str>,
    iterm_session_id: Option<&str>,
) -> String {
    let tmux = tmux_pane.unwrap_or("");
    let iterm = iterm_session_id.unwrap_or("");
    let input = format!("{tty}|{tmux}|{iterm}");
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

#[derive(Debug, Clone)]
struct TerminalIdentity {
    tty: String,
    tmux_pane: Option<String>,
    iterm_session_id: Option<String>,
}

fn current_terminal_identity() -> Result<TerminalIdentity> {
    let tty = current_tty()?;
    let tmux_pane = std::env::var("TMUX_PANE").ok();
    let iterm_session_id = std::env::var("ITERM_SESSION_ID").ok();
    Ok(TerminalIdentity {
        tty,
        tmux_pane,
        iterm_session_id,
    })
}

pub fn current_term_key() -> Result<String> {
    let identity = current_terminal_identity()?;
    Ok(compute_term_key(
        &identity.tty,
        identity.tmux_pane.as_deref(),
        identity.iterm_session_id.as_deref(),
    ))
}

fn term_key_from_env() -> Option<String> {
    if let Ok(key) = std::env::var("AGENTEXPORT_TERM") {
        if !key.trim().is_empty() {
            return Some(key);
        }
    }
    if let Ok(key) = std::env::var("AGENTEXPORT_TERM_KEY") {
        if !key.trim().is_empty() {
            return Some(key);
        }
    }
    None
}

fn resolve_term_key_from_env_or_tty() -> Result<String> {
    if let Some(key) = term_key_from_env() {
        return Ok(key);
    }
    current_term_key()
}

fn current_tty() -> Result<String> {
    unsafe {
        let ptr = libc::ttyname(libc::STDIN_FILENO);
        if !ptr.is_null() {
            let c_str = CStr::from_ptr(ptr);
            return Ok(c_str.to_str()?.to_string());
        }

        let dev_tty = CString::new("/dev/tty")?;
        let fd = libc::open(dev_tty.as_ptr(), libc::O_RDONLY);
        if fd < 0 {
            bail!("stdin is not a tty and /dev/tty unavailable; pass --term-key explicitly");
        }
        let ptr = libc::ttyname(fd);
        libc::close(fd);
        if ptr.is_null() {
            bail!("failed to resolve tty; pass --term-key explicitly");
        }
        let c_str = CStr::from_ptr(ptr);
        Ok(c_str.to_str()?.to_string())
    }
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn append_env_exports(path: &Path, state: &ClaudeState) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    writeln!(
        file,
        "export AGENTEXPORT_TERM={}",
        shell_quote(&state.term_key)
    )?;
    writeln!(
        file,
        "export AGENTEXPORT_TERM_KEY={}",
        shell_quote(&state.term_key)
    )?;
    writeln!(
        file,
        "export AGENTEXPORT_CLAUDE_SESSION_ID={}",
        shell_quote(&state.session_id)
    )?;
    writeln!(
        file,
        "export AGENTEXPORT_CLAUDE_TRANSCRIPT_PATH={}",
        shell_quote(&state.transcript_path)
    )?;
    Ok(())
}

fn extract_string_field(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let obj = value.as_object()?;
    for key in keys {
        if let Some(val) = obj.get(*key) {
            if let Some(s) = val.as_str() {
                return Some(s.to_string());
            }
        }
    }
    None
}

pub fn handle_claude_sessionstart(input: &str) -> Result<ClaudeState> {
    let value: serde_json::Value = serde_json::from_str(input).context("invalid JSON")?;
    let session_id = extract_string_field(&value, &["session_id", "sessionId", "session", "id"])
        .context("missing session_id")?;
    let transcript_path =
        extract_string_field(&value, &["transcript_path", "transcriptPath", "transcript"])
            .context("missing transcript_path")?;
    let cwd =
        extract_string_field(&value, &["cwd", "working_dir", "workingDir"]).unwrap_or_default();
    let term_key = current_term_key()?;
    let state = ClaudeState {
        term_key: term_key.clone(),
        session_id,
        transcript_path,
        cwd,
        updated_at: now_unix(),
    };
    write_claude_state(&state)?;
    if let Ok(env_file) = std::env::var("CLAUDE_ENV_FILE") {
        append_env_exports(Path::new(&env_file), &state)?;
    } else {
        eprintln!("CLAUDE_ENV_FILE not set; wrote state only");
    }
    Ok(state)
}

pub fn write_claude_state(state: &ClaudeState) -> Result<PathBuf> {
    let dir = state_dir(Tool::Claude)?;
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", state.term_key));
    let data = serde_json::to_string_pretty(state)?;
    fs::write(&path, data)?;
    Ok(path)
}

pub fn read_claude_state(term_key: &str) -> Result<ClaudeState> {
    let path = claude_state_path(term_key)?;
    let data =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let state = serde_json::from_str(&data)?;
    Ok(state)
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

fn file_contains(path: &Path, needle: &str, max_bytes: usize) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; max_bytes];
    let n = file.read(&mut buf)?;
    let content = String::from_utf8_lossy(&buf[..n]);
    Ok(content.contains(needle))
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

fn validate_transcript_fresh(path: &Path, max_age_minutes: u64) -> Result<(u64, u64)> {
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

fn default_gzip_path(tool: Tool, term_key: &str) -> Result<PathBuf> {
    let dir = cache_dir()?.join(APP_NAME).join("tmp");
    fs::create_dir_all(&dir)?;
    let filename = format!("{}-{}-{}.jsonl.gz", tool.as_str(), term_key, now_unix());
    Ok(dir.join(filename))
}

fn gzip_to_file(input: &Path, output: &Path) -> Result<u64> {
    let mut reader = File::open(input)?;
    let writer = File::create(output)?;
    let mut encoder = GzEncoder::new(writer, Compression::default());
    let bytes = std::io::copy(&mut reader, &mut encoder)?;
    encoder.finish()?;
    Ok(bytes)
}

fn default_render_path(tool: Tool, term_key: &str) -> Result<PathBuf> {
    let dir = cache_dir()?.join(APP_NAME).join("renders");
    fs::create_dir_all(&dir)?;
    let filename = format!("{}-{}-{}.json", tool.as_str(), term_key, now_unix());
    Ok(dir.join(filename))
}

fn format_generated_at_nice() -> String {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let month = match now.month() {
        time::Month::January => "Jan",
        time::Month::February => "Feb",
        time::Month::March => "Mar",
        time::Month::April => "Apr",
        time::Month::May => "May",
        time::Month::June => "Jun",
        time::Month::July => "Jul",
        time::Month::August => "Aug",
        time::Month::September => "Sep",
        time::Month::October => "Oct",
        time::Month::November => "Nov",
        time::Month::December => "Dec",
    };
    let hour = now.hour();
    let minute = now.minute();
    let (hour12, ampm) = if hour == 0 {
        (12, "am")
    } else if hour < 12 {
        (hour, "am")
    } else if hour == 12 {
        (12, "pm")
    } else {
        (hour - 12, "pm")
    };
    format!(
        "{} {}, {} {}:{:02}{}",
        month,
        now.day(),
        now.year(),
        hour12,
        minute,
        ampm
    )
}

fn truncate(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out = String::new();
    for (idx, ch) in input.chars().enumerate() {
        if idx >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn looks_like_internal_block(text: &str) -> bool {
    let trimmed = text.trim_start();
    if trimmed.starts_with("<environment_context>") {
        return true;
    }
    if trimmed.starts_with("<INSTRUCTIONS>") {
        return true;
    }
    if trimmed.starts_with("# AGENTS.md") {
        return true;
    }
    if trimmed.contains("\n<environment_context>") {
        return true;
    }
    if trimmed.contains("\n<INSTRUCTIONS>") {
        return true;
    }
    false
}

fn normalize_role(role: &str) -> String {
    let lower = role.trim().to_lowercase();
    if lower.contains("assistant") || lower == "model" {
        "assistant".to_string()
    } else if lower.contains("user") || lower.contains("human") {
        "user".to_string()
    } else if lower.contains("system") {
        "system".to_string()
    } else if lower.contains("tool") || lower.contains("function") {
        "tool".to_string()
    } else {
        lower
    }
}

#[allow(dead_code)]
fn role_from_type(value: &str) -> Option<String> {
    let lower = value.to_lowercase();
    if lower.contains("assistant") || lower.contains("model") {
        Some("assistant".to_string())
    } else if lower.contains("user") || lower.contains("human") {
        Some("user".to_string())
    } else if lower.contains("system") {
        Some("system".to_string())
    } else if lower.contains("tool") || lower.contains("function") {
        Some("tool".to_string())
    } else {
        None
    }
}

#[allow(dead_code)]
fn extract_role(value: &Value) -> Option<String> {
    if let Some(role) = value.get("role").and_then(|v| v.as_str()) {
        return Some(normalize_role(role));
    }
    if let Some(role) = value.pointer("/message/role").and_then(|v| v.as_str()) {
        return Some(normalize_role(role));
    }
    if let Some(role) = value.get("speaker").and_then(|v| v.as_str()) {
        return Some(normalize_role(role));
    }
    if let Some(role) = value.pointer("/author/role").and_then(|v| v.as_str()) {
        return Some(normalize_role(role));
    }
    if value.get("tool_calls").is_some()
        || value.get("tool_call").is_some()
        || value.get("function_call").is_some()
    {
        return Some("tool".to_string());
    }
    if let Some(typ) = value.get("type").and_then(|v| v.as_str()) {
        if let Some(role) = role_from_type(typ) {
            return Some(role);
        }
    }
    if let Some(event) = value.get("event").and_then(|v| v.as_str()) {
        if let Some(role) = role_from_type(event) {
            return Some(role);
        }
    }
    None
}

fn extract_text(value: &Value, depth: usize) -> Option<String> {
    if depth > 6 {
        return None;
    }
    match value {
        Value::String(text) => Some(text.to_string()),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                if let Some(part) = extract_text(item, depth + 1) {
                    if !part.trim().is_empty() {
                        parts.push(part);
                    }
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(|v| v.as_str()) {
                return Some(text.to_string());
            }
            if let Some(content) = map.get("content") {
                if let Some(text) = extract_text(content, depth + 1) {
                    return Some(text);
                }
            }
            if let Some(value) = map.get("value") {
                if let Some(text) = extract_text(value, depth + 1) {
                    return Some(text);
                }
            }
            if let Some(delta) = map.get("delta") {
                if let Some(text) = extract_text(delta, depth + 1) {
                    return Some(text);
                }
            }
            if let Some(message) = map.get("message") {
                if let Some(text) = extract_text(message, depth + 1) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}

fn format_tool_call(value: &Value) -> String {
    let name = value
        .get("name")
        .and_then(|v| v.as_str())
        .or_else(|| value.pointer("/function/name").and_then(|v| v.as_str()))
        .or_else(|| value.pointer("/tool/name").and_then(|v| v.as_str()))
        .unwrap_or("tool");
    let mut out = format!("tool: {name}");
    if let Some(id) = value.get("id").and_then(|v| v.as_str()) {
        out.push_str(&format!("\nid: {id}"));
    }
    if let Some(args) = value
        .get("arguments")
        .or_else(|| value.get("args"))
        .or_else(|| value.pointer("/function/arguments"))
    {
        let args_text = if let Some(text) = args.as_str() {
            text.to_string()
        } else {
            serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string())
        };
        out.push_str("\nargs:\n");
        out.push_str(&args_text);
    }
    out
}

fn tool_summary(value: &Value) -> String {
    let name = value
        .get("name")
        .and_then(|v| v.as_str())
        .or_else(|| value.pointer("/function/name").and_then(|v| v.as_str()))
        .or_else(|| value.pointer("/tool/name").and_then(|v| v.as_str()))
        .or_else(|| value.get("tool").and_then(|v| v.as_str()))
        .unwrap_or("tool");
    if value.get("output").is_some()
        || value.get("result").is_some()
        || value.get("response").is_some()
    {
        format!("Tool response: {name}")
    } else {
        format!("Tool call: {name}")
    }
}

fn is_tool_payload(value: &Value) -> bool {
    if value.get("tool_calls").is_some()
        || value.get("tool_call").is_some()
        || value.get("function_call").is_some()
        || value.get("tool_result").is_some()
        || value.get("tool_output").is_some()
    {
        return true;
    }
    if let Some(typ) = value.get("type").and_then(|v| v.as_str()) {
        let lower = typ.to_lowercase();
        if lower.contains("tool") || lower.contains("function") {
            return true;
        }
    }
    false
}

fn format_tool_calls(value: &Value) -> String {
    if let Some(items) = value.as_array() {
        let mut parts = Vec::new();
        for item in items {
            parts.push(format_tool_call(item));
        }
        if !parts.is_empty() {
            return parts.join("\n\n");
        }
    }
    format_tool_call(value)
}

fn extract_content(value: &Value) -> Option<String> {
    if let Some(content) = value.get("content") {
        if let Some(text) = extract_text(content, 0) {
            return Some(text);
        }
    }
    if let Some(message) = value.get("message") {
        if let Some(content) = message.get("content") {
            if let Some(text) = extract_text(content, 0) {
                return Some(text);
            }
        }
        if let Some(text) = extract_text(message, 0) {
            return Some(text);
        }
    }
    for key in ["text", "delta", "output_text", "input_text", "message_text"] {
        if let Some(value) = value.get(key) {
            if let Some(text) = extract_text(value, 0) {
                return Some(text);
            }
        }
    }
    if let Some(output) = value.get("output") {
        if let Some(text) = extract_text(output, 0) {
            return Some(text);
        }
    }
    if let Some(input) = value.get("input") {
        if let Some(text) = extract_text(input, 0) {
            return Some(text);
        }
    }
    if let Some(tool_calls) = value.get("tool_calls") {
        return Some(format_tool_calls(tool_calls));
    }
    if let Some(tool_call) = value
        .get("tool_call")
        .or_else(|| value.get("function_call"))
    {
        return Some(format_tool_call(tool_call));
    }
    None
}

#[allow(dead_code)]
fn summarize_event(value: &Value) -> Option<String> {
    if let Some(kind) = value.get("type").and_then(|v| v.as_str()) {
        return Some(format!("event: {kind}"));
    }
    if let Some(kind) = value.get("event").and_then(|v| v.as_str()) {
        return Some(format!("event: {kind}"));
    }
    if let Some(kind) = value.get("name").and_then(|v| v.as_str()) {
        return Some(format!("event: {kind}"));
    }
    None
}

fn extract_transcript_meta(path: &Path) -> TranscriptMeta {
    let mut meta = TranscriptMeta::default();
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return meta,
    };
    let reader = BufReader::new(file);

    for line in reader.lines().take(100) {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Claude: extract compaction summary (NOT for title, just for display)
        if meta.compaction_summary.is_none() {
            if value.get("type").and_then(|v| v.as_str()) == Some("summary") {
                if let Some(summary) = value.get("summary").and_then(|v| v.as_str()) {
                    meta.compaction_summary = Some(summary.to_string());
                }
            }
        }

        // Claude: look for slug field on user messages
        if meta.slug.is_none() {
            if let Some(slug) = value.get("slug").and_then(|v| v.as_str()) {
                meta.slug = Some(slug.to_string());
            }
        }

        // Extract first user message content
        if meta.first_user_message.is_none() {
            let is_user = value.get("type").and_then(|v| v.as_str()) == Some("user")
                || value.pointer("/message/role").and_then(|v| v.as_str()) == Some("user")
                || value.get("role").and_then(|v| v.as_str()) == Some("user");
            if is_user {
                if let Some(content) = value
                    .pointer("/message/content")
                    .and_then(|v| v.as_str())
                    .or_else(|| value.get("content").and_then(|v| v.as_str()))
                {
                    let trimmed = content.trim();
                    if !trimmed.is_empty() && !looks_like_internal_block(trimmed) {
                        // Truncate to reasonable title length
                        let title = if trimmed.len() > 100 {
                            format!("{}...", &trimmed[..100])
                        } else {
                            trimmed.to_string()
                        };
                        meta.first_user_message = Some(title);
                    }
                }
            }
        }

        // Stop early if we have what we need
        if meta.compaction_summary.is_some() && meta.first_user_message.is_some() {
            break;
        }
    }

    meta
}

/// Token usage for a single message
#[derive(Debug, Clone, Default)]
struct MessageUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
}

/// Result of parsing a transcript
#[derive(Debug, Default)]
struct ParseResult {
    messages: Vec<RenderedMessage>,
    /// Model usage counts for determining dominant model
    model_counts: HashMap<String, usize>,
    /// Token usage by message ID (deduplicated - later values overwrite earlier)
    usage_by_message_id: HashMap<String, MessageUsage>,
    /// Token usage totals (for Codex cumulative totals, not deduplicated)
    codex_total_input_tokens: u64,
    codex_total_output_tokens: u64,
    codex_total_cache_read_tokens: u64,
}

impl ParseResult {
    /// Get sorted list of models by usage (most used first)
    fn models_by_usage(&self) -> Vec<String> {
        let mut models: Vec<_> = self.model_counts.iter().collect();
        models.sort_by(|a, b| b.1.cmp(a.1));
        models.into_iter().map(|(k, _)| k.clone()).collect()
    }

    /// Get the dominant (most used) model
    fn dominant_model(&self) -> Option<String> {
        self.models_by_usage().into_iter().next()
    }

    /// Compute total input tokens (Claude: sum deduplicated, Codex: use cumulative)
    fn total_input_tokens(&self) -> u64 {
        if self.codex_total_input_tokens > 0 {
            self.codex_total_input_tokens
        } else {
            self.usage_by_message_id
                .values()
                .map(|u| u.input_tokens)
                .sum()
        }
    }

    /// Compute total output tokens
    fn total_output_tokens(&self) -> u64 {
        if self.codex_total_output_tokens > 0 {
            self.codex_total_output_tokens
        } else {
            self.usage_by_message_id
                .values()
                .map(|u| u.output_tokens)
                .sum()
        }
    }

    /// Compute total cache read tokens
    fn total_cache_read_tokens(&self) -> u64 {
        if self.codex_total_cache_read_tokens > 0 {
            self.codex_total_cache_read_tokens
        } else {
            self.usage_by_message_id
                .values()
                .map(|u| u.cache_read_tokens)
                .sum()
        }
    }

    /// Compute total cache creation tokens
    fn total_cache_creation_tokens(&self) -> u64 {
        self.usage_by_message_id
            .values()
            .map(|u| u.cache_creation_tokens)
            .sum()
    }
}

fn parse_transcript(path: &Path) -> Result<ParseResult> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut result = ParseResult::default();
    let mut codex_mode = false;
    let mut current_model: Option<String> = None;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // Detect Codex mode
        if event_type == "session_meta" {
            if value
                .pointer("/payload/originator")
                .and_then(|v| v.as_str())
                == Some("codex_cli_rs")
            {
                codex_mode = true;
            }
            continue;
        }

        // Skip internal events (but process event_msg in Codex mode for token usage)
        if matches!(
            event_type,
            "file-history-snapshot" | "summary" | "queue-operation"
        ) {
            continue;
        }
        if event_type == "event_msg" && !codex_mode {
            continue;
        }

        // ===== CODEX FORMAT =====
        if codex_mode {
            // Track model from turn_context
            if event_type == "turn_context" {
                if let Some(model) = value.pointer("/payload/model").and_then(|v| v.as_str()) {
                    current_model = Some(model.to_string());
                }
                continue;
            }

            // Extract token usage from event_msg (Codex reports cumulative totals)
            if event_type == "event_msg" {
                if let Some(payload_type) = value.pointer("/payload/type").and_then(|v| v.as_str())
                {
                    if payload_type == "token_count" {
                        if let Some(usage) = value.pointer("/payload/info/total_token_usage") {
                            if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64())
                            {
                                result.codex_total_input_tokens = input; // cumulative total
                            }
                            if let Some(output) =
                                usage.get("output_tokens").and_then(|v| v.as_u64())
                            {
                                result.codex_total_output_tokens = output;
                            }
                            if let Some(cached) =
                                usage.get("cached_input_tokens").and_then(|v| v.as_u64())
                            {
                                result.codex_total_cache_read_tokens = cached;
                            }
                        }
                    }
                }
                continue;
            }

            if event_type != "response_item" {
                continue;
            }
            if let Some(payload) = value.get("payload") {
                let payload_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if payload_type == "message" {
                    let role = payload
                        .get("role")
                        .and_then(|v| v.as_str())
                        .map(normalize_role)
                        .unwrap_or_else(|| "assistant".to_string());

                    // Check for images in content array
                    if let Some(content_arr) = payload.get("content").and_then(|v| v.as_array()) {
                        for block in content_arr {
                            if block.get("type").and_then(|t| t.as_str()) == Some("input_image") {
                                result.messages.push(RenderedMessage {
                                    role: role.clone(),
                                    content: "[Image]".to_string(),
                                    raw: None,
                                    raw_label: None,
                                    tool_use_id: None,
                                    model: current_model.clone(),
                                });
                            }
                        }
                    }

                    let content = extract_content(payload).unwrap_or_default();
                    if !content.trim().is_empty() && !looks_like_internal_block(&content) {
                        let model = current_model.clone();
                        if let Some(ref m) = model {
                            *result.model_counts.entry(m.clone()).or_insert(0) += 1;
                        }
                        result.messages.push(RenderedMessage {
                            role,
                            content,
                            raw: None,
                            raw_label: None,
                            tool_use_id: None,
                            model,
                        });
                    }
                } else if payload_type == "function_call" {
                    let name = payload
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool");
                    let call_id = payload
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let args = payload.get("arguments");
                    let content = if let Some(a) = args {
                        let pretty = serde_json::to_string_pretty(a).unwrap_or_default();
                        format!("{}\n{}", name, truncate(&pretty, 2000))
                    } else {
                        name.to_string()
                    };
                    let raw = serde_json::to_string_pretty(payload)
                        .ok()
                        .map(|t| truncate(&t, 20000));
                    result.messages.push(RenderedMessage {
                        role: "tool".to_string(),
                        content,
                        raw,
                        raw_label: Some("Results".to_string()),
                        tool_use_id: call_id,
                        model: None,
                    });
                } else if payload_type == "function_call_output" {
                    let call_id = payload
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let output = payload
                        .get("output")
                        .and_then(|v| v.as_str())
                        .unwrap_or("[output]");
                    result.messages.push(RenderedMessage {
                        role: "tool".to_string(),
                        content: truncate(output, 500),
                        raw: None,
                        raw_label: None,
                        tool_use_id: call_id,
                        model: None,
                    });
                } else if payload_type == "reasoning" {
                    // Codex reasoning/thinking - extract summary text (full content is encrypted)
                    if let Some(summary_arr) = payload.get("summary").and_then(|v| v.as_array()) {
                        let summary_text: Vec<String> = summary_arr
                            .iter()
                            .filter_map(|item| {
                                if item.get("type").and_then(|t| t.as_str()) == Some("summary_text")
                                {
                                    item.get("text")
                                        .and_then(|t| t.as_str())
                                        .map(|s| s.to_string())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        if !summary_text.is_empty() {
                            result.messages.push(RenderedMessage {
                                role: "thinking".to_string(),
                                content: summary_text.join("\n"),
                                raw: None,
                                raw_label: None,
                                tool_use_id: None,
                                model: current_model.clone(),
                            });
                        }
                    }
                } else if is_tool_payload(payload) {
                    let content = tool_summary(payload);
                    let raw = serde_json::to_string_pretty(payload)
                        .ok()
                        .map(|t| truncate(&t, 20000));
                    let tool_id = payload
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    result.messages.push(RenderedMessage {
                        role: "tool".to_string(),
                        content,
                        raw,
                        raw_label: Some("Tool payload".to_string()),
                        tool_use_id: tool_id,
                        model: None,
                    });
                }
            }
            continue;
        }

        // ===== CLAUDE FORMAT =====
        match event_type {
            "user" => {
                // User message: message.content is a string
                if let Some(content) = value.pointer("/message/content").and_then(|v| v.as_str()) {
                    // Skip internal/system messages
                    if content.starts_with("Caveat:")
                        || content.starts_with("Unknown slash command:")
                        || content.starts_with("This slash command can only be invoked")
                        || content.trim().is_empty()
                        || looks_like_internal_block(content)
                    {
                        continue;
                    }
                    // Compaction/summary messages should be system role (hidden with tool calls)
                    let role = if content.contains("conversation is summarized below")
                        || content.contains("continued from a previous conversation")
                    {
                        "system"
                    } else {
                        "user"
                    };
                    result.messages.push(RenderedMessage {
                        role: role.to_string(),
                        content: content.to_string(),
                        raw: None,
                        raw_label: None,
                        tool_use_id: None,
                        model: None,
                    });
                }
            }
            "assistant" => {
                // Extract model from message.model
                let model = value
                    .pointer("/message/model")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(ref m) = model {
                    *result.model_counts.entry(m.clone()).or_insert(0) += 1;
                }

                // Extract token usage from message.usage, deduplicated by message.id
                // Claude streams multiple updates for the same message ID - use last values
                if let Some(usage) = value.pointer("/message/usage") {
                    let msg_id = value
                        .pointer("/message/id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let input = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let output = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cache_read = usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cache_create = usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);

                    // Overwrite - later updates have final values
                    result.usage_by_message_id.insert(
                        msg_id,
                        MessageUsage {
                            input_tokens: input,
                            output_tokens: output,
                            cache_read_tokens: cache_read,
                            cache_creation_tokens: cache_create,
                        },
                    );
                }

                // Assistant message: message.content is array of blocks
                if let Some(content_arr) =
                    value.pointer("/message/content").and_then(|v| v.as_array())
                {
                    for block in content_arr {
                        let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match block_type {
                            "text" => {
                                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                    if !text.trim().is_empty() {
                                        result.messages.push(RenderedMessage {
                                            role: "assistant".to_string(),
                                            content: text.to_string(),
                                            raw: None,
                                            raw_label: None,
                                            tool_use_id: None,
                                            model: model.clone(),
                                        });
                                    }
                                }
                            }
                            "tool_use" => {
                                let name =
                                    block.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                                let tool_id = block
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                let input = block.get("input");
                                let content = if let Some(inp) = input {
                                    let pretty =
                                        serde_json::to_string_pretty(inp).unwrap_or_default();
                                    format!("{}\n{}", name, truncate(&pretty, 2000))
                                } else {
                                    name.to_string()
                                };
                                let raw = serde_json::to_string_pretty(block)
                                    .ok()
                                    .map(|t| truncate(&t, 20000));
                                result.messages.push(RenderedMessage {
                                    role: "tool".to_string(),
                                    content,
                                    raw,
                                    raw_label: Some("Results".to_string()),
                                    tool_use_id: tool_id,
                                    model: None,
                                });
                            }
                            "tool_result" => {
                                let tool_id = block
                                    .get("tool_use_id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                let content = block
                                    .get("content")
                                    .and_then(|v| v.as_str())
                                    .or_else(|| block.get("output").and_then(|v| v.as_str()))
                                    .unwrap_or("[result]");
                                result.messages.push(RenderedMessage {
                                    role: "tool".to_string(),
                                    content: truncate(content, 500),
                                    raw: None,
                                    raw_label: None,
                                    tool_use_id: tool_id,
                                    model: None,
                                });
                            }
                            "thinking" => {
                                if let Some(thinking_text) =
                                    block.get("thinking").and_then(|v| v.as_str())
                                {
                                    if !thinking_text.trim().is_empty() {
                                        result.messages.push(RenderedMessage {
                                            role: "thinking".to_string(),
                                            content: thinking_text.to_string(),
                                            raw: None,
                                            raw_label: None,
                                            tool_use_id: None,
                                            model: model.clone(),
                                        });
                                    }
                                }
                            }
                            "image" => {
                                // Placeholder for images - don't include base64 data
                                result.messages.push(RenderedMessage {
                                    role: "assistant".to_string(),
                                    content: "[Image]".to_string(),
                                    raw: None,
                                    raw_label: None,
                                    tool_use_id: None,
                                    model: model.clone(),
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
            "system" => {
                // System messages - skip most, they're internal
            }
            _ => {
                // Unknown event type - skip
            }
        }
    }

    Ok(result)
}

fn create_share_payload(
    tool: Tool,
    transcript_path: &Path,
    session_id: Option<&str>,
    thread_id: Option<&str>,
    title_override: Option<&str>,
) -> Result<SharePayload> {
    let parsed = parse_transcript(transcript_path)?;
    let meta = extract_transcript_meta(transcript_path);

    let tool_display = match tool {
        Tool::Claude => "Claude Code",
        Tool::Codex => "Codex",
    };

    let title = title_override
        .map(|s| s.to_string())
        .or(meta.slug.map(|s| s.replace('-', " ")))
        .or(meta.first_user_message);

    let models = parsed.models_by_usage();
    let total_input = parsed.total_input_tokens();
    let total_output = parsed.total_output_tokens();
    let total_cache_read = parsed.total_cache_read_tokens();
    let total_cache_creation = parsed.total_cache_creation_tokens();

    Ok(SharePayload {
        tool: tool_display.to_string(),
        session_id: session_id.or(thread_id).map(|s| s.to_string()),
        title,
        compaction_summary: meta.compaction_summary,
        shared_at: format_generated_at_nice(),
        model: parsed.dominant_model(),
        models,
        messages: parsed.messages,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_cache_read_tokens: total_cache_read,
        total_cache_creation_tokens: total_cache_creation,
    })
}

fn resolve_claude_transcript(
    term_key: &str,
    transcript_arg: Option<PathBuf>,
) -> Result<(PathBuf, Option<String>)> {
    if let Some(path) = transcript_arg {
        if let Ok(session_id) = std::env::var("AGENTEXPORT_CLAUDE_SESSION_ID") {
            return Ok((path, Some(session_id)));
        }
        if let Ok(session_id) = std::env::var("TRANSCRIPTCTL_CLAUDE_SESSION_ID") {
            return Ok((path, Some(session_id)));
        }
        if let Ok(state) = read_claude_state(term_key) {
            return Ok((path, Some(state.session_id)));
        }
        return Ok((path, None));
    }
    if let Ok(path) = std::env::var("AGENTEXPORT_CLAUDE_TRANSCRIPT_PATH") {
        let session_id = std::env::var("AGENTEXPORT_CLAUDE_SESSION_ID").ok();
        if session_id.is_some() {
            return Ok((PathBuf::from(path), session_id));
        }
        if let Ok(state) = read_claude_state(term_key) {
            return Ok((PathBuf::from(path), Some(state.session_id)));
        }
        return Ok((PathBuf::from(path), None));
    }
    if let Ok(path) = std::env::var("TRANSCRIPTCTL_CLAUDE_TRANSCRIPT_PATH") {
        let session_id = std::env::var("TRANSCRIPTCTL_CLAUDE_SESSION_ID").ok();
        if session_id.is_some() {
            return Ok((PathBuf::from(path), session_id));
        }
        if let Ok(state) = read_claude_state(term_key) {
            return Ok((PathBuf::from(path), Some(state.session_id)));
        }
        return Ok((PathBuf::from(path), None));
    }
    let state = read_claude_state(term_key)
        .context("missing claude state; run claude-sessionstart first")?;
    Ok((PathBuf::from(state.transcript_path), Some(state.session_id)))
}

fn resolve_codex_transcript(
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

fn find_codex_transcript_for_cwd_from_history(
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

pub fn publish(options: PublishOptions) -> Result<PublishResult> {
    let term_key = match options.tool {
        Tool::Claude => match options.term_key {
            Some(key) => key,
            None => resolve_term_key_from_env_or_tty()?,
        },
        Tool::Codex => options.term_key.unwrap_or_else(|| "codex".to_string()),
    };

    let (transcript_path, session_id, thread_id) = match options.tool {
        Tool::Claude => {
            let (path, session_id) = resolve_claude_transcript(&term_key, options.transcript)?;
            (path, session_id, None)
        }
        Tool::Codex => {
            let (path, thread_id) =
                resolve_codex_transcript(options.transcript, options.max_age_minutes)?;
            (path, None, thread_id)
        }
    };

    let (input_bytes, modified_at) =
        validate_transcript_fresh(&transcript_path, options.max_age_minutes)?;

    if let Some(session_id) = session_id.as_ref() {
        let filename = transcript_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if !filename.contains(session_id) {
            bail!("transcript filename does not include session_id");
        }
    }

    if let Some(thread_id) = thread_id.as_ref() {
        if !file_contains(&transcript_path, thread_id, 128 * 1024)? {
            bail!("transcript does not contain thread-id");
        }
    }

    let gzip_path = match options.out {
        Some(path) => path,
        None => default_gzip_path(options.tool, &term_key)?,
    };
    fs::create_dir_all(gzip_path.parent().unwrap_or_else(|| Path::new(".")))?;
    gzip_to_file(&transcript_path, &gzip_path)?;
    let gzip_bytes = fs::metadata(&gzip_path)?.len();

    // Create payload if uploading or rendering
    let should_create_payload = options.render || options.upload_url.is_some();
    let (render_path, payload_json) = if should_create_payload {
        let payload = create_share_payload(
            options.tool,
            &transcript_path,
            session_id.as_deref(),
            thread_id.as_deref(),
            options.title.as_deref(),
        )?;
        let json = serde_json::to_string(&payload)?;

        // Only write to disk if --render was explicitly requested
        let path = if options.render {
            let render_path = default_render_path(options.tool, &term_key)?;
            fs::create_dir_all(render_path.parent().unwrap_or_else(|| Path::new(".")))?;
            // Write JSON for local preview (can be viewed with a local viewer)
            fs::write(&render_path, &json)?;
            Some(render_path.display().to_string())
        } else {
            None
        };
        (path, Some(json))
    } else {
        (None, None)
    };

    // Handle upload
    let (share_url, note) = if options.dry_run {
        (None, "upload skipped (dry-run)".to_string())
    } else if options.upload_url.is_none() {
        (None, "upload skipped (no upload_url)".to_string())
    } else if options.storage_type == StorageType::Gist {
        let json = payload_json.expect("Payload should be created for upload");
        let description = format!(
            "agentexport share ({}, {})",
            options.tool.as_str(),
            format_generated_at_nice()
        );
        let result = upload::upload_gist("gist", &json, &description, options.gist_format)?;

        // Save share locally for management
        let share_url = result.share_url.clone();
        let share = shares::Share {
            id: result.id,
            key: result.key,
            delete_token: result.delete_token,
            upload_url: result.upload_url,
            share_url: Some(share_url),
            created_at: time::OffsetDateTime::now_utc(),
            expires_at: time::OffsetDateTime::from_unix_timestamp(result.expires_at as i64)
                .unwrap_or_else(|_| time::OffsetDateTime::now_utc()),
            tool: options.tool.as_str().to_string(),
            transcript_path: transcript_path.display().to_string(),
            storage_type: options.storage_type,
        };
        shares::save_share(&share)?;

        (Some(result.share_url), "uploaded successfully".to_string())
    } else if let Some(upload_url) = &options.upload_url {
        let json = payload_json.expect("Payload should be created for upload");
        let encrypted = crypto::encrypt_html(&json)?;
        let result = upload::upload_blob(
            upload_url,
            &encrypted.blob,
            &encrypted.key_b64,
            options.ttl_days,
        )?;

        // Save share locally for management
        let share_url = result.share_url.clone();
        let share = shares::Share {
            id: result.id,
            key: result.key,
            delete_token: result.delete_token,
            upload_url: result.upload_url,
            share_url: Some(share_url),
            created_at: time::OffsetDateTime::now_utc(),
            expires_at: time::OffsetDateTime::from_unix_timestamp(result.expires_at as i64)
                .unwrap_or_else(|_| time::OffsetDateTime::now_utc()),
            tool: options.tool.as_str().to_string(),
            transcript_path: transcript_path.display().to_string(),
            storage_type: options.storage_type,
        };
        shares::save_share(&share)?;

        (Some(result.share_url), "uploaded successfully".to_string())
    } else {
        (None, "upload skipped (no upload_url)".to_string())
    };

    Ok(PublishResult {
        status: "ready".to_string(),
        tool: options.tool.as_str().to_string(),
        term_key,
        transcript_path: transcript_path.display().to_string(),
        gzip_path: gzip_path.display().to_string(),
        input_bytes,
        gzip_bytes,
        modified_at,
        session_id,
        thread_id,
        render_path,
        share_url,
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    struct EnvGuard {
        key: String,
        old: Option<String>,
    }

    struct DirGuard {
        original: PathBuf,
    }

    impl DirGuard {
        fn set(path: &Path) -> Result<Self> {
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

    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key: key.to_string(),
                old,
            }
        }

        fn clear(key: &str) -> Self {
            let old = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
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

    #[test]
    fn term_key_hash_is_stable() {
        let key = compute_term_key("/dev/ttys007", Some("%1"), Some("ABC"));
        assert_eq!(
            key,
            "dab577fe0a6ec2761d461d687ee15471967cefa6d697e24f40f53db872caf1d7"
        );
    }

    #[test]
    fn cache_dir_respects_env_override() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _guard = EnvGuard::set("AGENTEXPORT_CACHE_DIR", tmp.path().to_str().unwrap());
        let dir = cache_dir().unwrap();
        assert_eq!(dir, tmp.path());
    }

    #[test]
    fn resolve_term_key_prefers_env() {
        let _lock = env_lock();
        let _guard = EnvGuard::set("AGENTEXPORT_TERM", "test-key");
        let key = resolve_term_key_from_env_or_tty().unwrap();
        assert_eq!(key, "test-key");
    }

    #[test]
    fn write_and_read_claude_state_roundtrip() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _guard = EnvGuard::set("AGENTEXPORT_CACHE_DIR", tmp.path().to_str().unwrap());
        let state = ClaudeState {
            term_key: "abc".to_string(),
            session_id: "sess".to_string(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/work".to_string(),
            updated_at: 123,
        };
        let path = write_claude_state(&state).unwrap();
        assert!(path.exists());
        let loaded = read_claude_state("abc").unwrap();
        assert_eq!(loaded.session_id, "sess");
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
    fn publish_renders_share_payload() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _guard = EnvGuard::set("AGENTEXPORT_CACHE_DIR", tmp.path().to_str().unwrap());
        let _guard_session = EnvGuard::set("AGENTEXPORT_CLAUDE_SESSION_ID", "");
        let transcript = tmp.path().join("sample.jsonl");
        // Use Claude format with type field
        fs::write(
            &transcript,
            concat!(
                "{\"type\":\"user\",\"message\":{\"content\":\"Hello\"}}\n",
                "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Hi\"}]}}\n"
            ),
        )
        .unwrap();

        let result = publish(PublishOptions {
            tool: Tool::Claude,
            term_key: Some("term".to_string()),
            transcript: Some(transcript),
            max_age_minutes: 10,
            out: None,
            dry_run: true,
            upload_url: None,
            render: true,
            ttl_days: 30,
            storage_type: StorageType::Agentexport,
            gist_format: GistFormat::Markdown,
            title: None,
        })
        .unwrap();

        let render_path = result.render_path.expect("render path");
        let json = fs::read_to_string(render_path).unwrap();
        assert!(json.contains("\"tool\":\"Claude Code\""));
        assert!(json.contains("Hello"));
        assert!(json.contains("\"role\":\"assistant\""));
    }

    #[test]
    fn publish_claude_uses_state_transcript() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _guard = EnvGuard::set("AGENTEXPORT_CACHE_DIR", tmp.path().to_str().unwrap());
        // Clear env vars that would override the state lookup
        let _guard2 = EnvGuard::clear("AGENTEXPORT_CLAUDE_TRANSCRIPT_PATH");
        let _guard3 = EnvGuard::clear("AGENTEXPORT_CLAUDE_SESSION_ID");

        let transcript = tmp.path().join("sess-abc.jsonl");
        fs::write(&transcript, "{\"role\":\"user\",\"content\":\"Hello\"}\n").unwrap();

        let state = ClaudeState {
            term_key: "term".to_string(),
            session_id: "sess-abc".to_string(),
            transcript_path: transcript.display().to_string(),
            cwd: "/work".to_string(),
            updated_at: 1,
        };
        write_claude_state(&state).unwrap();

        let result = publish(PublishOptions {
            tool: Tool::Claude,
            term_key: Some("term".to_string()),
            transcript: None,
            max_age_minutes: 0,
            out: None,
            dry_run: true,
            upload_url: None,
            render: false,
            ttl_days: 30,
            storage_type: StorageType::Agentexport,
            gist_format: GistFormat::Markdown,
            title: None,
        })
        .unwrap();

        assert_eq!(result.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(PathBuf::from(&result.transcript_path), transcript);
    }

    #[test]
    fn publish_codex_uses_history_for_current_cwd() {
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

        let result = publish(PublishOptions {
            tool: Tool::Codex,
            term_key: None,
            transcript: None,
            max_age_minutes: 0,
            out: None,
            dry_run: true,
            upload_url: None,
            render: false,
            ttl_days: 30,
            storage_type: StorageType::Agentexport,
            gist_format: GistFormat::Markdown,
            title: None,
        })
        .unwrap();

        assert_eq!(result.thread_id.as_deref(), Some(session_id));
        assert_eq!(PathBuf::from(&result.transcript_path), session_path);
    }

    #[test]
    fn publish_codex_fails_without_history() {
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

        let err = publish(PublishOptions {
            tool: Tool::Codex,
            term_key: None,
            transcript: None,
            max_age_minutes: 0,
            out: None,
            dry_run: true,
            upload_url: None,
            render: false,
            ttl_days: 30,
            storage_type: StorageType::Agentexport,
            gist_format: GistFormat::Markdown,
            title: None,
        })
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("unable to resolve codex transcript from history")
        );
    }

    #[test]
    fn parse_codex_response_item_messages() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("codex.jsonl");
        let data = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"abc\",\"originator\":\"codex_cli_rs\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Hi\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello\"}]}}\n"
        );
        fs::write(&path, data).unwrap();
        let result = parse_transcript(&path).unwrap();
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, "user");
        assert_eq!(result.messages[0].content, "Hi");
        assert_eq!(result.messages[1].role, "assistant");
        assert_eq!(result.messages[1].content, "Hello");
    }

    #[test]
    fn filters_internal_blocks() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("codex.jsonl");
        let data = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"originator\":\"codex_cli_rs\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"<environment_context>\\n  <cwd>/tmp</cwd>\\n</environment_context>\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Real question\"}]}}\n"
        );
        fs::write(&path, data).unwrap();
        let result = parse_transcript(&path).unwrap();
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].content, "Real question");
    }

    #[test]
    fn validate_claude_filename_check() {
        let tmp = TempDir::new().unwrap();
        let transcript = tmp.path().join("sess-123.jsonl");
        fs::write(&transcript, "{}").unwrap();
        let (bytes, _mtime) = validate_transcript_fresh(&transcript, 10).unwrap();
        assert_eq!(bytes, 2);
        let filename = transcript.file_name().and_then(|s| s.to_str()).unwrap();
        assert!(filename.contains("sess-123"));
    }

    #[test]
    fn parse_claude_thinking_blocks() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("claude.jsonl");
        let data = r#"{"type":"assistant","message":{"model":"claude-sonnet-4","content":[{"type":"thinking","thinking":"Let me analyze this..."},{"type":"text","text":"Here is my answer"}]}}"#;
        fs::write(&path, data).unwrap();

        let result = parse_transcript(&path).unwrap();
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, "thinking");
        assert_eq!(result.messages[0].content, "Let me analyze this...");
        assert_eq!(result.messages[1].role, "assistant");
        assert_eq!(result.messages[1].content, "Here is my answer");
    }

    #[test]
    fn parse_claude_image_placeholder() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("claude.jsonl");
        let data = r#"{"type":"assistant","message":{"model":"claude-sonnet-4","content":[{"type":"image","source":{"type":"base64","data":"abc123"}},{"type":"text","text":"As shown above"}]}}"#;
        fs::write(&path, data).unwrap();

        let result = parse_transcript(&path).unwrap();
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, "assistant");
        assert_eq!(result.messages[0].content, "[Image]");
        assert_eq!(result.messages[1].content, "As shown above");
    }

    #[test]
    fn parse_claude_token_usage() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("claude.jsonl");
        // Two different messages with different IDs - usage is summed
        let data = concat!(
            r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4","usage":{"input_tokens":1000,"output_tokens":500,"cache_read_input_tokens":800,"cache_creation_input_tokens":200},"content":[{"type":"text","text":"Hello"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"id":"msg_2","model":"claude-sonnet-4","usage":{"input_tokens":1500,"output_tokens":300,"cache_read_input_tokens":1200,"cache_creation_input_tokens":0},"content":[{"type":"text","text":"World"}]}}"#
        );
        fs::write(&path, data).unwrap();

        let result = parse_transcript(&path).unwrap();
        assert_eq!(result.total_input_tokens(), 2500);
        assert_eq!(result.total_output_tokens(), 800);
        assert_eq!(result.total_cache_read_tokens(), 2000);
        assert_eq!(result.total_cache_creation_tokens(), 200);
    }

    #[test]
    fn parse_claude_token_usage_dedup() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("claude.jsonl");
        // Same message ID streamed multiple times - only last values count
        let data = concat!(
            r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4","usage":{"input_tokens":100,"output_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"content":[{"type":"text","text":"H"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"content":[{"type":"text","text":"Hello"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4","usage":{"input_tokens":100,"output_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"content":[{"type":"text","text":"Hello World"}]}}"#
        );
        fs::write(&path, data).unwrap();

        let result = parse_transcript(&path).unwrap();
        // Should use final values (100, 100), not sum (100+100+100)
        assert_eq!(result.total_input_tokens(), 100);
        assert_eq!(result.total_output_tokens(), 100);
    }

    #[test]
    fn parse_codex_reasoning_summary() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("codex.jsonl");
        let data = concat!(
            r#"{"type":"session_meta","payload":{"originator":"codex_cli_rs"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"**Analyzing the code**"}],"encrypted_content":"abc123"}}"#
        );
        fs::write(&path, data).unwrap();

        let result = parse_transcript(&path).unwrap();
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].role, "thinking");
        assert_eq!(result.messages[0].content, "**Analyzing the code**");
    }

    #[test]
    fn parse_codex_model_from_turn_context() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("codex.jsonl");
        let data = concat!(
            r#"{"type":"session_meta","payload":{"originator":"codex_cli_rs"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-5","cwd":"/test"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello"}]}}"#
        );
        fs::write(&path, data).unwrap();

        let result = parse_transcript(&path).unwrap();
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].model, Some("gpt-5".to_string()));
        assert!(result.model_counts.contains_key("gpt-5"));
    }

    #[test]
    fn share_payload_includes_token_usage() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("claude.jsonl");
        let data = r#"{"type":"assistant","message":{"model":"claude-sonnet-4","usage":{"input_tokens":1000,"output_tokens":500},"content":[{"type":"text","text":"Hello"}]}}"#;
        fs::write(&path, data).unwrap();

        let payload = create_share_payload(Tool::Claude, &path, None, None, None).unwrap();
        assert_eq!(payload.total_input_tokens, 1000);
        assert_eq!(payload.total_output_tokens, 500);
    }

    #[test]
    fn parse_codex_token_usage() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("codex.jsonl");
        let data = concat!(
            r#"{"type":"session_meta","payload":{"originator":"codex_cli_rs"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":200,"output_tokens":500}}}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2500,"cached_input_tokens":800,"output_tokens":1200}}}}"#
        );
        fs::write(&path, data).unwrap();

        let result = parse_transcript(&path).unwrap();
        // Should have final totals (Codex reports cumulative totals)
        assert_eq!(result.total_input_tokens(), 2500);
        assert_eq!(result.total_output_tokens(), 1200);
        assert_eq!(result.total_cache_read_tokens(), 800);
    }

    #[test]
    fn parse_codex_image_placeholder() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("codex.jsonl");
        let data = concat!(
            r#"{"type":"session_meta","payload":{"originator":"codex_cli_rs"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,abc"},{"type":"input_text","text":"What is this?"}]}}"#
        );
        fs::write(&path, data).unwrap();

        let result = parse_transcript(&path).unwrap();
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].content, "[Image]");
        assert_eq!(result.messages[1].content, "What is this?");
    }
}
