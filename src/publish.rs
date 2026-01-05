//! Publish orchestration: main workflow for exporting transcripts.

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;

use crate::config::{GistFormat, StorageType};
use crate::crypto;
use crate::shares;
use crate::terminal::shell_quote;
use crate::transcript::{
    Tool, SharePayload, cache_dir, extract_transcript_meta, file_contains, parse_transcript,
    resolve_transcript, validate_transcript_fresh,
};
use crate::upload;

const APP_NAME: &str = "agentexport";

/// Claude session state (legacy, for hook integration)
#[derive(Debug, Serialize, Deserialize)]
pub struct ClaudeState {
    pub term_key: String,
    pub session_id: String,
    pub transcript_path: String,
    pub cwd: String,
    pub updated_at: u64,
}

/// Options for the publish command
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

/// Result of the publish command
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

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn state_dir(tool: Tool) -> Result<PathBuf> {
    Ok(cache_dir()?.join(APP_NAME).join(tool.as_str()))
}

/// Get the path to a Claude state file for a given term_key
pub fn claude_state_path(term_key: &str) -> Result<PathBuf> {
    Ok(state_dir(Tool::Claude)?.join(format!("{term_key}.json")))
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

/// Handle the claude-sessionstart hook
pub fn handle_claude_sessionstart(input: &str) -> Result<ClaudeState> {
    let value: serde_json::Value = serde_json::from_str(input).context("invalid JSON")?;
    let session_id = extract_string_field(&value, &["session_id", "sessionId", "session", "id"])
        .context("missing session_id")?;
    let transcript_path =
        extract_string_field(&value, &["transcript_path", "transcriptPath", "transcript"])
            .context("missing transcript_path")?;
    let cwd =
        extract_string_field(&value, &["cwd", "working_dir", "workingDir"]).unwrap_or_default();
    let term_key = crate::terminal::current_term_key()?;
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

/// Write Claude state to disk
pub fn write_claude_state(state: &ClaudeState) -> Result<PathBuf> {
    let dir = state_dir(Tool::Claude)?;
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", state.term_key));
    let data = serde_json::to_string_pretty(state)?;
    fs::write(&path, data)?;
    Ok(path)
}

/// Read Claude state from disk
pub fn read_claude_state(term_key: &str) -> Result<ClaudeState> {
    let path = claude_state_path(term_key)?;
    let data =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let state = serde_json::from_str(&data)?;
    Ok(state)
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

fn create_share_payload(
    tool: Tool,
    transcript_path: &Path,
    session_id: Option<&str>,
    thread_id: Option<&str>,
    title_override: Option<&str>,
) -> Result<SharePayload> {
    let parsed = parse_transcript(transcript_path)?;
    let meta = extract_transcript_meta(transcript_path);

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
        tool: tool.display_name().to_string(),
        session_id: session_id.or(thread_id).map(|s| s.to_string()),
        title,
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

/// Main publish workflow
pub fn publish(options: PublishOptions) -> Result<PublishResult> {
    let term_key = options.term_key.unwrap_or_else(|| match options.tool {
        Tool::Claude => "claude".to_string(),
        Tool::Codex => "codex".to_string(),
    });

    let (transcript_path, session_id, thread_id) =
        resolve_transcript(options.tool, options.transcript, options.max_age_minutes)?;

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
            created_at: OffsetDateTime::now_utc(),
            expires_at: OffsetDateTime::from_unix_timestamp(result.expires_at as i64)
                .unwrap_or_else(|_| OffsetDateTime::now_utc()),
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
            created_at: OffsetDateTime::now_utc(),
            expires_at: OffsetDateTime::from_unix_timestamp(result.expires_at as i64)
                .unwrap_or_else(|_| OffsetDateTime::now_utc()),
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
    use crate::test_utils::{env_lock, DirGuard, EnvGuard};
    use crate::transcript::cwd_to_project_folder;
    use tempfile::TempDir;

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
    fn publish_claude_finds_transcript_by_cwd() {
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

        let result = publish(PublishOptions {
            tool: Tool::Claude,
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

        assert_eq!(result.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(PathBuf::from(&result.transcript_path), transcript);
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

        assert!(err
            .to_string()
            .contains("unable to resolve codex transcript from history"));
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

    // ===== extract_string_field tests =====

    #[test]
    fn test_extract_string_field_first_match() {
        let json = serde_json::json!({"id": "123", "session_id": "456"});
        assert_eq!(
            extract_string_field(&json, &["session_id", "id"]),
            Some("456".to_string())
        );
    }

    #[test]
    fn test_extract_string_field_fallback() {
        let json = serde_json::json!({"id": "123"});
        assert_eq!(
            extract_string_field(&json, &["session_id", "id"]),
            Some("123".to_string())
        );
    }

    #[test]
    fn test_extract_string_field_none() {
        let json = serde_json::json!({"foo": "bar"});
        assert_eq!(extract_string_field(&json, &["id"]), None);
    }

    #[test]
    fn test_extract_string_field_not_string() {
        let json = serde_json::json!({"id": 123});
        assert_eq!(extract_string_field(&json, &["id"]), None);
    }

    #[test]
    fn test_extract_string_field_null() {
        let json = serde_json::json!({"id": null});
        assert_eq!(extract_string_field(&json, &["id"]), None);
    }

    #[test]
    fn test_extract_string_field_not_object() {
        let json = serde_json::json!("just a string");
        assert_eq!(extract_string_field(&json, &["id"]), None);
    }
}
