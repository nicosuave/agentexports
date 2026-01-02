use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use time::{OffsetDateTime, format_description};
use walkdir::WalkDir;

mod skills;

pub use skills::setup_skills_interactive;

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

#[derive(Debug, Serialize, Deserialize)]
pub struct CodexState {
    pub term_key: String,
    pub thread_id: String,
    pub cwd: String,
    pub updated_at: u64,
    #[serde(default)]
    pub tty: Option<String>,
    #[serde(default)]
    pub tmux_pane: Option<String>,
    #[serde(default)]
    pub iterm_session_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
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
    pub note: String,
}

#[derive(Debug, Clone)]
struct RenderedMessage {
    role: String,
    content: String,
    raw: Option<String>,
    raw_label: Option<String>,
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

fn default_notify_log_path() -> Result<PathBuf> {
    Ok(cache_dir()?.join(APP_NAME).join("notify.log"))
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

pub fn codex_state_path(term_key: &str) -> Result<PathBuf> {
    Ok(state_dir(Tool::Codex)?.join(format!("{term_key}.json")))
}

pub fn compute_term_key(tty: &str, tmux_pane: Option<&str>, iterm_session_id: Option<&str>) -> String {
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
    let cwd = extract_string_field(&value, &["cwd", "working_dir", "workingDir"]).unwrap_or_default();
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

pub fn handle_codex_notify(input: &str, term_key_override: Option<String>) -> Result<CodexState> {
    let value: serde_json::Value = serde_json::from_str(input).context("invalid JSON")?;
    let thread_id = extract_string_field(&value, &["thread-id", "thread_id", "threadId"]);
    let session_id = extract_string_field(&value, &["session-id", "session_id", "sessionId"]);
    let thread_id = match (thread_id, session_id.as_ref()) {
        (Some(thread_id), _) => thread_id,
        (None, Some(session_id)) => session_id.clone(),
        (None, None) => bail!("missing thread-id/session-id"),
    };
    let cwd = extract_string_field(&value, &["cwd", "working_dir", "workingDir"]).unwrap_or_default();
    let identity = current_terminal_identity()?;
    let term_key = match term_key_override {
        Some(key) => key,
        None => compute_term_key(
            &identity.tty,
            identity.tmux_pane.as_deref(),
            identity.iterm_session_id.as_deref(),
        ),
    };
    let state = CodexState {
        term_key: term_key.clone(),
        thread_id,
        cwd,
        updated_at: now_unix(),
        tty: Some(identity.tty),
        tmux_pane: identity.tmux_pane,
        iterm_session_id: identity.iterm_session_id,
        session_id,
    };
    write_codex_state(&state)?;
    let payload = serde_json::json!({
        "timestamp": now_unix(),
        "term_key": state.term_key,
        "thread_id": state.thread_id,
        "session_id": state.session_id,
        "cwd": state.cwd,
        "tty": state.tty,
        "tmux_pane": state.tmux_pane,
        "iterm_session_id": state.iterm_session_id,
        "raw": value,
    });
    let log_paths = [
        default_notify_log_path().ok(),
        std::env::var("AGENTEXPORT_NOTIFY_LOG").ok().map(PathBuf::from),
    ];
    for path in log_paths.into_iter().flatten() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{}", payload);
        }
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

pub fn write_codex_state(state: &CodexState) -> Result<PathBuf> {
    let dir = state_dir(Tool::Codex)?;
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", state.term_key));
    let data = serde_json::to_string_pretty(state)?;
    fs::write(&path, data)?;
    Ok(path)
}

pub fn read_claude_state(term_key: &str) -> Result<ClaudeState> {
    let path = claude_state_path(term_key)?;
    let data = fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let state = serde_json::from_str(&data)?;
    Ok(state)
}

pub fn read_codex_state(term_key: &str) -> Result<CodexState> {
    let path = codex_state_path(term_key)?;
    let data = fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
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
    let meta = fs::metadata(path)
        .with_context(|| format!("missing transcript: {}", path.display()))?;
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
    let filename = format!("{}-{}-{}.html", tool.as_str(), term_key, now_unix());
    Ok(dir.join(filename))
}

fn format_generated_at() -> String {
    let fmt = format_description::parse(
        "[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour sign:mandatory][offset_minute]",
    )
    .ok();
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    if let Some(fmt) = fmt {
        if let Ok(text) = now.format(&fmt) {
            return text;
        }
    }
    now_unix().to_string()
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

fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
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
    for key in [
        "text",
        "delta",
        "output_text",
        "input_text",
        "message_text",
    ] {
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
    if let Some(tool_call) = value.get("tool_call").or_else(|| value.get("function_call")) {
        return Some(format_tool_call(tool_call));
    }
    None
}

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

fn parse_transcript(path: &Path) -> Result<Vec<RenderedMessage>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    let mut codex_mode = false;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => {
                let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if event_type == "session_meta" {
                    if let Some(originator) = value
                        .get("payload")
                        .and_then(|p| p.get("originator"))
                        .and_then(|v| v.as_str())
                    {
                        if originator == "codex_cli_rs" {
                            codex_mode = true;
                        }
                    }
                    continue;
                }
                if event_type == "event_msg" {
                    continue;
                }
                let payload = value.get("payload");
                let primary = payload.unwrap_or(&value);

                if codex_mode && event_type != "response_item" {
                    continue;
                }

                if event_type == "response_item" {
                    if let Some(payload) = payload {
                        let payload_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if payload_type == "message" {
                            let role = payload
                                .get("role")
                                .and_then(|v| v.as_str())
                                .map(normalize_role)
                                .unwrap_or_else(|| "assistant".to_string());
                            let content = extract_content(payload).unwrap_or_default();
                            if !content.trim().is_empty() && !looks_like_internal_block(&content) {
                                messages.push(RenderedMessage {
                                    role,
                                    content,
                                    raw: None,
                                    raw_label: None,
                                });
                            }
                            continue;
                        }
                        if is_tool_payload(payload) {
                            let content = tool_summary(payload);
                            let raw = serde_json::to_string_pretty(payload)
                                .ok()
                                .map(|text| truncate(&text, 20000));
                            messages.push(RenderedMessage {
                                role: "tool".to_string(),
                                content,
                                raw,
                                raw_label: Some("Tool payload".to_string()),
                            });
                            continue;
                        }
                    }
                    continue;
                }

                let role = extract_role(primary)
                    .or_else(|| extract_role(&value))
                    .unwrap_or_else(|| "event".to_string());
                let mut content = extract_content(primary)
                    .or_else(|| extract_content(&value))
                    .unwrap_or_default();
                let mut raw = None;
                let mut raw_label = None;

                if role == "tool" {
                    content = tool_summary(primary);
                    if let Ok(pretty) = serde_json::to_string_pretty(primary) {
                        raw = Some(truncate(&pretty, 20000));
                        raw_label = Some("Tool payload".to_string());
                    } else {
                        raw = Some(truncate(trimmed, 8000));
                        raw_label = Some("Tool payload".to_string());
                    }
                } else if content.trim().is_empty() {
                    if let Some(summary) = summarize_event(primary)
                        .or_else(|| summarize_event(&value))
                    {
                        content = summary;
                    } else {
                        content = "[unparsed event]".to_string();
                    }
                }

                if raw.is_none() && (role == "event" || content == "[unparsed event]") {
                    raw = Some(truncate(trimmed, 8000));
                    raw_label = Some("Raw event".to_string());
                }

                if !looks_like_internal_block(&content) {
                    messages.push(RenderedMessage {
                        role,
                        content,
                        raw,
                        raw_label,
                    });
                }
            }
            Err(_) => {
                messages.push(RenderedMessage {
                    role: "event".to_string(),
                    content: trimmed.to_string(),
                    raw: None,
                    raw_label: None,
                });
            }
        }
    }

    Ok(messages)
}

fn render_share_page(
    tool: Tool,
    term_key: &str,
    transcript_path: &Path,
    session_id: Option<&str>,
    thread_id: Option<&str>,
    cwd: Option<&str>,
) -> Result<String> {
    let messages = parse_transcript(transcript_path)?;
    let generated_at = format_generated_at();
    let message_count = messages.len();

    let mut role_counts: BTreeMap<String, usize> = BTreeMap::new();
    for message in &messages {
        *role_counts.entry(message.role.clone()).or_insert(0) += 1;
    }
    let mut role_summary = String::new();
    for (role, count) in role_counts {
        if !role_summary.is_empty() {
            role_summary.push_str(" | ");
        }
        role_summary.push_str(&format!("{role} {count}"));
    }

    let file_name = transcript_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("transcript.jsonl");

    let mut meta_rows = Vec::new();
    meta_rows.push(("tool", tool.as_str().to_string()));
    meta_rows.push(("term key", term_key.to_string()));
    if let Some(session_id) = session_id {
        meta_rows.push(("session", session_id.to_string()));
    }
    if let Some(thread_id) = thread_id {
        meta_rows.push(("thread", thread_id.to_string()));
    }
    if let Some(cwd) = cwd {
        if !cwd.trim().is_empty() {
            meta_rows.push(("cwd", cwd.to_string()));
        }
    }
    meta_rows.push(("source", file_name.to_string()));
    meta_rows.push(("generated", generated_at));

    let mut html = String::new();
    html.push_str("<!doctype html>\n");
    html.push_str("<html lang=\"en\">\n");
    html.push_str("<head>\n");
    html.push_str("  <meta charset=\"utf-8\" />\n");
    html.push_str("  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n");
    html.push_str("  <title>Agent Export</title>\n");
    html.push_str("  <style>\n");
    html.push_str("    :root {\n");
    html.push_str("      --ink: #1f1b18;\n");
    html.push_str("      --muted: #6a625b;\n");
    html.push_str("      --paper: #f8f3ee;\n");
    html.push_str("      --panel: #fffaf4;\n");
    html.push_str("      --accent: #c36b3d;\n");
    html.push_str("      --accent-2: #2b7b6f;\n");
    html.push_str("      --assistant: #ffffff;\n");
    html.push_str("      --user: #ffe7d2;\n");
    html.push_str("      --system: #e9f0fb;\n");
    html.push_str("      --tool: #e6f6f0;\n");
    html.push_str("      --event: #f2ede7;\n");
    html.push_str("      --border: #e2d6cb;\n");
    html.push_str("      --shadow: 0 16px 40px rgba(25, 20, 16, 0.08);\n");
    html.push_str("      --radius: 18px;\n");
    html.push_str("    }\n");
    html.push_str("    * { box-sizing: border-box; }\n");
    html.push_str("    body {\n");
    html.push_str("      margin: 0;\n");
    html.push_str("      font-family: \"Space Grotesk\", \"Avenir Next\", \"Segoe UI\", sans-serif;\n");
    html.push_str("      color: var(--ink);\n");
    html.push_str("      background:\n");
    html.push_str("        radial-gradient(1200px 600px at 15% -10%, #fff3e6 0%, transparent 55%),\n");
    html.push_str("        radial-gradient(900px 500px at 100% 10%, #e8f5f1 0%, transparent 50%),\n");
    html.push_str("        linear-gradient(180deg, #f6efe7 0%, #fdf9f5 60%, #faf4ee 100%);\n");
    html.push_str("      min-height: 100vh;\n");
    html.push_str("    }\n");
    html.push_str("    main {\n");
    html.push_str("      max-width: 980px;\n");
    html.push_str("      margin: 0 auto;\n");
    html.push_str("      padding: 48px 24px 80px;\n");
    html.push_str("    }\n");
    html.push_str("    header.hero {\n");
    html.push_str("      background: linear-gradient(135deg, rgba(255,255,255,0.92), rgba(255,248,241,0.95));\n");
    html.push_str("      border: 1px solid var(--border);\n");
    html.push_str("      border-radius: calc(var(--radius) + 6px);\n");
    html.push_str("      box-shadow: var(--shadow);\n");
    html.push_str("      padding: 28px 32px 24px;\n");
    html.push_str("      position: relative;\n");
    html.push_str("      overflow: hidden;\n");
    html.push_str("    }\n");
    html.push_str("    header.hero::after {\n");
    html.push_str("      content: \"\";\n");
    html.push_str("      position: absolute;\n");
    html.push_str("      inset: -40% 60% auto -20%;\n");
    html.push_str("      height: 220px;\n");
    html.push_str("      background: radial-gradient(circle, rgba(195,107,61,0.18), transparent 70%);\n");
    html.push_str("      pointer-events: none;\n");
    html.push_str("    }\n");
    html.push_str("    .title {\n");
    html.push_str("      font-size: 32px;\n");
    html.push_str("      letter-spacing: -0.02em;\n");
    html.push_str("      margin: 0 0 6px;\n");
    html.push_str("    }\n");
    html.push_str("    .subtitle {\n");
    html.push_str("      margin: 0;\n");
    html.push_str("      color: var(--muted);\n");
    html.push_str("      font-size: 15px;\n");
    html.push_str("    }\n");
    html.push_str("    .meta {\n");
    html.push_str("      display: flex;\n");
    html.push_str("      flex-wrap: wrap;\n");
    html.push_str("      gap: 8px;\n");
    html.push_str("      margin-top: 18px;\n");
    html.push_str("    }\n");
    html.push_str("    .chip {\n");
    html.push_str("      display: inline-flex;\n");
    html.push_str("      align-items: center;\n");
    html.push_str("      gap: 8px;\n");
    html.push_str("      padding: 6px 12px;\n");
    html.push_str("      border-radius: 999px;\n");
    html.push_str("      border: 1px solid var(--border);\n");
    html.push_str("      background: rgba(255,255,255,0.72);\n");
    html.push_str("      font-size: 12px;\n");
    html.push_str("      color: var(--muted);\n");
    html.push_str("    }\n");
    html.push_str("    .chip strong { color: var(--ink); font-weight: 600; }\n");
    html.push_str("    .stats {\n");
    html.push_str("      margin: 28px 0 22px;\n");
    html.push_str("      display: grid;\n");
    html.push_str("      grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));\n");
    html.push_str("      gap: 12px;\n");
    html.push_str("    }\n");
    html.push_str("    .stat {\n");
    html.push_str("      padding: 16px 18px;\n");
    html.push_str("      background: var(--panel);\n");
    html.push_str("      border: 1px solid var(--border);\n");
    html.push_str("      border-radius: var(--radius);\n");
    html.push_str("      box-shadow: 0 10px 24px rgba(22, 17, 13, 0.06);\n");
    html.push_str("    }\n");
    html.push_str("    .stat-label { font-size: 12px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.08em; }\n");
    html.push_str("    .stat-value { margin-top: 6px; font-size: 16px; font-weight: 600; }\n");
    html.push_str("    .messages {\n");
    html.push_str("      display: flex;\n");
    html.push_str("      flex-direction: column;\n");
    html.push_str("      gap: 18px;\n");
    html.push_str("    }\n");
    html.push_str("    .msg {\n");
    html.push_str("      width: min(92%, 760px);\n");
    html.push_str("      border-radius: var(--radius);\n");
    html.push_str("      border: 1px solid var(--border);\n");
    html.push_str("      padding: 16px 18px 14px;\n");
    html.push_str("      background: var(--assistant);\n");
    html.push_str("      box-shadow: 0 12px 24px rgba(24, 18, 14, 0.06);\n");
    html.push_str("      animation: rise 0.5s ease both;\n");
    html.push_str("      animation-delay: calc(var(--i) * 0.04s);\n");
    html.push_str("    }\n");
    html.push_str("    .msg.user { margin-left: auto; background: var(--user); }\n");
    html.push_str("    .msg.system { background: var(--system); }\n");
    html.push_str("    .msg.tool { background: var(--tool); }\n");
    html.push_str("    .msg.event { background: var(--event); }\n");
    html.push_str("    .msg-head {\n");
    html.push_str("      display: flex;\n");
    html.push_str("      justify-content: space-between;\n");
    html.push_str("      align-items: center;\n");
    html.push_str("      margin-bottom: 10px;\n");
    html.push_str("      font-size: 12px;\n");
    html.push_str("      color: var(--muted);\n");
    html.push_str("      text-transform: uppercase;\n");
    html.push_str("      letter-spacing: 0.08em;\n");
    html.push_str("    }\n");
    html.push_str("    .badge {\n");
    html.push_str("      padding: 4px 10px;\n");
    html.push_str("      border-radius: 999px;\n");
    html.push_str("      background: rgba(31, 27, 24, 0.08);\n");
    html.push_str("      color: var(--ink);\n");
    html.push_str("      font-size: 11px;\n");
    html.push_str("      font-weight: 600;\n");
    html.push_str("    }\n");
    html.push_str("    .msg-content {\n");
    html.push_str("      white-space: pre-wrap;\n");
    html.push_str("      font-size: 15px;\n");
    html.push_str("      line-height: 1.5;\n");
    html.push_str("    }\n");
    html.push_str("    .raw {\n");
    html.push_str("      margin-top: 12px;\n");
    html.push_str("    }\n");
    html.push_str("    .divider {\n");
    html.push_str("      margin: 22px 0 6px;\n");
    html.push_str("      display: flex;\n");
    html.push_str("      align-items: center;\n");
    html.push_str("      gap: 12px;\n");
    html.push_str("      color: var(--muted);\n");
    html.push_str("      font-size: 12px;\n");
    html.push_str("      letter-spacing: 0.12em;\n");
    html.push_str("      text-transform: uppercase;\n");
    html.push_str("    }\n");
    html.push_str("    .divider::before,\n");
    html.push_str("    .divider::after {\n");
    html.push_str("      content: \"\";\n");
    html.push_str("      flex: 1;\n");
    html.push_str("      height: 1px;\n");
    html.push_str("      background: var(--border);\n");
    html.push_str("    }\n");
    html.push_str("    .raw summary {\n");
    html.push_str("      cursor: pointer;\n");
    html.push_str("      color: var(--accent-2);\n");
    html.push_str("      font-size: 12px;\n");
    html.push_str("      text-transform: uppercase;\n");
    html.push_str("      letter-spacing: 0.08em;\n");
    html.push_str("    }\n");
    html.push_str("    .raw pre {\n");
    html.push_str("      background: rgba(31, 27, 24, 0.08);\n");
    html.push_str("      padding: 12px;\n");
    html.push_str("      border-radius: 12px;\n");
    html.push_str("      overflow-x: auto;\n");
    html.push_str("      font-size: 12px;\n");
    html.push_str("      line-height: 1.4;\n");
    html.push_str("    }\n");
    html.push_str("    footer {\n");
    html.push_str("      margin-top: 40px;\n");
    html.push_str("      color: var(--muted);\n");
    html.push_str("      font-size: 12px;\n");
    html.push_str("      text-align: center;\n");
    html.push_str("    }\n");
    html.push_str("    @keyframes rise {\n");
    html.push_str("      from { opacity: 0; transform: translateY(12px); }\n");
    html.push_str("      to { opacity: 1; transform: translateY(0); }\n");
    html.push_str("    }\n");
    html.push_str("    @media (max-width: 720px) {\n");
    html.push_str("      main { padding: 32px 16px 60px; }\n");
    html.push_str("      .msg { width: 100%; }\n");
    html.push_str("      .title { font-size: 26px; }\n");
    html.push_str("    }\n");
    html.push_str("    @media (prefers-reduced-motion: reduce) {\n");
    html.push_str("      .msg { animation: none; }\n");
    html.push_str("    }\n");
    html.push_str("  </style>\n");
    html.push_str("</head>\n");
    html.push_str("<body>\n");
    html.push_str("  <main>\n");
    html.push_str("    <header class=\"hero\">\n");
    html.push_str("      <h1 class=\"title\">Agent Export</h1>\n");
    html.push_str("      <p class=\"subtitle\">Chat session share page</p>\n");
    html.push_str("      <div class=\"meta\">\n");
    for (label, value) in meta_rows {
        html.push_str("        <span class=\"chip\"><strong>");
        html.push_str(&escape_html(label));
        html.push_str("</strong> ");
        html.push_str(&escape_html(&value));
        html.push_str("</span>\n");
    }
    html.push_str("      </div>\n");
    html.push_str("    </header>\n");
    html.push_str("    <section class=\"stats\">\n");
    html.push_str("      <div class=\"stat\"><div class=\"stat-label\">messages</div><div class=\"stat-value\">");
    html.push_str(&message_count.to_string());
    html.push_str("</div></div>\n");
    html.push_str("      <div class=\"stat\"><div class=\"stat-label\">roles</div><div class=\"stat-value\">");
    html.push_str(&escape_html(&role_summary));
    html.push_str("</div></div>\n");
    html.push_str("    </section>\n");
    html.push_str("    <section class=\"messages\">\n");

    if messages.is_empty() {
        html.push_str("      <div class=\"msg event\" style=\"--i:0;\">\n");
        html.push_str("        <div class=\"msg-head\"><span class=\"badge\">empty</span><span>#0</span></div>\n");
        html.push_str("        <div class=\"msg-content\">No messages were parsed from this transcript.</div>\n");
        html.push_str("      </div>\n");
    } else {
        let mut last_role: Option<String> = None;
        for (idx, message) in messages.iter().enumerate() {
            let role = if message.role.is_empty() {
                "event"
            } else {
                message.role.as_str()
            };
            if last_role.as_deref() != Some(role) {
                html.push_str("      <div class=\"divider\">");
                html.push_str(&escape_html(role));
                html.push_str("</div>\n");
                last_role = Some(role.to_string());
            }
            let role_class = match role {
                "user" => "user",
                "assistant" => "assistant",
                "system" => "system",
                "tool" => "tool",
                _ => "event",
            };
            html.push_str(&format!("      <article class=\"msg {role_class}\" data-role=\"{}\" style=\"--i:{};\">\n", escape_html(role), idx));
            html.push_str("        <div class=\"msg-head\">\n");
            html.push_str("          <span class=\"badge\">");
            html.push_str(&escape_html(role));
            html.push_str("</span>\n");
            html.push_str(&format!("          <span>#{}", idx + 1));
            html.push_str("</span>\n");
            html.push_str("        </div>\n");
            html.push_str("        <div class=\"msg-content\">");
            html.push_str(&escape_html(&message.content));
            html.push_str("</div>\n");
            if let Some(raw) = &message.raw {
                html.push_str("        <details class=\"raw\">\n");
                let label = message.raw_label.as_deref().unwrap_or("Raw event");
                html.push_str("          <summary>");
                html.push_str(&escape_html(label));
                html.push_str("</summary>\n");
                html.push_str("          <pre>");
                html.push_str(&escape_html(raw));
                html.push_str("</pre>\n");
                html.push_str("        </details>\n");
            }
            html.push_str("      </article>\n");
        }
    }

    html.push_str("    </section>\n");
    html.push_str("    <footer>Generated by agentexport</footer>\n");
    html.push_str("  </main>\n");
    html.push_str("</body>\n");
    html.push_str("</html>\n");
    Ok(html)
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

    bail!("unable to resolve codex transcript from history; ensure history is enabled and run from the Codex session cwd, or pass --transcript");
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
    fs::create_dir_all(
        gzip_path
            .parent()
            .unwrap_or_else(|| Path::new(".")),
    )?;
    gzip_to_file(&transcript_path, &gzip_path)?;
    let gzip_bytes = fs::metadata(&gzip_path)?.len();

    let render_path = if options.render {
        let cwd = match options.tool {
            Tool::Claude => read_claude_state(&term_key).ok().map(|state| state.cwd),
            Tool::Codex => read_session_meta(&transcript_path)
                .ok()
                .and_then(|meta| meta.and_then(|meta| meta.cwd)),
        };
        let render_path = default_render_path(options.tool, &term_key)?;
        fs::create_dir_all(
            render_path
                .parent()
                .unwrap_or_else(|| Path::new(".")),
        )?;
        let html = render_share_page(
            options.tool,
            &term_key,
            &transcript_path,
            session_id.as_deref(),
            thread_id.as_deref(),
            cwd.as_deref(),
        )?;
        fs::write(&render_path, html)?;
        Some(render_path.display().to_string())
    } else {
        None
    };

    let note = if options.dry_run || options.upload_url.is_none() {
        "upload skipped (no upload_url or dry-run)".to_string()
    } else {
        "upload not implemented yet".to_string()
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
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
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
            Self { key: key.to_string(), old }
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
        let _guard_sessions = EnvGuard::set("AGENTEXPORT_CODEX_SESSIONS_DIR", tmp.path().to_str().unwrap());
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
    fn publish_renders_share_page() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _guard = EnvGuard::set("AGENTEXPORT_CACHE_DIR", tmp.path().to_str().unwrap());
        let _guard_session = EnvGuard::set("AGENTEXPORT_CLAUDE_SESSION_ID", "");
        let transcript = tmp.path().join("sample.jsonl");
        fs::write(
            &transcript,
            "{\"role\":\"user\",\"content\":\"Hello\"}\n{\"role\":\"assistant\",\"content\":\"Hi\"}\n",
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
        })
        .unwrap();

        let render_path = result.render_path.expect("render path");
        let html = fs::read_to_string(render_path).unwrap();
        assert!(html.contains("Agent Export"));
        assert!(html.contains("Hello"));
        assert!(html.contains("assistant"));
    }

    #[test]
    fn publish_claude_uses_state_transcript() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _guard = EnvGuard::set("AGENTEXPORT_CACHE_DIR", tmp.path().to_str().unwrap());

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

        let _guard_sessions =
            EnvGuard::set("AGENTEXPORT_CODEX_SESSIONS_DIR", sessions_dir.to_str().unwrap());
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

        let _guard_sessions =
            EnvGuard::set("AGENTEXPORT_CODEX_SESSIONS_DIR", sessions_dir.to_str().unwrap());
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
        })
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("unable to resolve codex transcript from history"));
    }

    #[test]
    fn parse_codex_response_item_messages() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("codex.jsonl");
        let data = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"abc\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Hi\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello\"}]}}\n"
        );
        fs::write(&path, data).unwrap();
        let messages = parse_transcript(&path).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "Hi");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "Hello");
    }

    #[test]
    fn filters_internal_blocks() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("codex.jsonl");
        let data = concat!(
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"<environment_context>\\n  <cwd>/tmp</cwd>\\n</environment_context>\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Real question\"}]}}\n"
        );
        fs::write(&path, data).unwrap();
        let messages = parse_transcript(&path).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "Real question");
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
}
