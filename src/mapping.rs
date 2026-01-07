//! Map transcript edits to git diff hunks for PR review tooling.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptTool {
    Claude,
    Codex,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct MappingMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub tool: TranscriptTool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MappingEdit {
    pub id: String,
    pub tool: TranscriptTool,
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_message_id: Option<String>,
    pub confidence: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffHunk {
    pub id: String,
    pub file_path: String,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditHunkLink {
    pub edit_id: String,
    pub hunk_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MappingResult {
    pub base: String,
    pub head: String,
    pub messages: Vec<MappingMessage>,
    pub edits: Vec<MappingEdit>,
    pub hunks: Vec<DiffHunk>,
    pub edit_hunks: Vec<EditHunkLink>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
struct RawEdit {
    id: String,
    tool: TranscriptTool,
    file_path: String,
    patch: Option<String>,
    old_string: Option<String>,
    new_string: Option<String>,
    message_id: Option<String>,
    user_message_id: Option<String>,
    timestamp: Option<String>,
    order: usize,
}

#[derive(Debug, Clone)]
struct PatchFile {
    path: String,
    hunks: Vec<PatchHunk>,
}

#[derive(Debug, Clone)]
struct PatchHunk {
    lines: Vec<HunkLine>,
}

#[derive(Debug, Clone)]
struct HunkLine {
    kind: char,
    text: String,
}

#[derive(Debug, Clone)]
struct FileState {
    lines: Vec<String>,
}

#[derive(Debug, Clone)]
struct AppliedRange {
    start: usize,
    end: usize,
    confidence: String,
}

pub struct MapOptions {
    pub transcripts: Vec<PathBuf>,
    pub repo: PathBuf,
    pub base: String,
    pub head: String,
}

pub fn map_transcripts(options: MapOptions) -> Result<MappingResult> {
    let mut messages = Vec::new();
    let mut edits = Vec::new();
    let mut errors = Vec::new();
    let mut order = 0usize;

    for path in &options.transcripts {
        let tool = detect_tool(path).unwrap_or(TranscriptTool::Unknown);
        match tool {
            TranscriptTool::Codex => {
                let (msgs, raw_edits) = parse_codex_transcript(path, &mut order)?;
                messages.extend(msgs);
                edits.extend(raw_edits);
            }
            TranscriptTool::Claude => {
                let (msgs, raw_edits) = parse_claude_transcript(path, &mut order)?;
                messages.extend(msgs);
                edits.extend(raw_edits);
            }
            TranscriptTool::Unknown => {
                errors.push(format!(
                    "unable to detect transcript tool for {}",
                    path.display()
                ));
            }
        }
    }

    edits.sort_by(|a, b| match (&a.timestamp, &b.timestamp) {
        (Some(a_ts), Some(b_ts)) => a_ts.cmp(b_ts).then_with(|| a.order.cmp(&b.order)),
        _ => a.order.cmp(&b.order),
    });

    let hunks = git_diff_hunks(&options.repo, &options.base, &options.head)
        .unwrap_or_else(|err| {
            errors.push(format!("git diff failed: {err}"));
            Vec::new()
        });

    let mut file_states: HashMap<String, FileState> = HashMap::new();
    let mut mapped_edits = Vec::new();

    for edit in &edits {
        let normalized_path = normalize_file_path(&options.repo, &edit.file_path);
        let state = file_states
            .entry(normalized_path.clone())
            .or_insert_with(|| load_base_file(&options.repo, &options.base, &normalized_path));

        let applied = if edit.tool == TranscriptTool::Codex {
            if let Some(patch) = &edit.patch {
                apply_codex_patch(state, patch, &normalized_path)
            } else {
                None
            }
        } else {
            apply_claude_edit(state, edit.old_string.as_deref(), edit.new_string.as_deref())
        };

        let (start_line, end_line, confidence) = match applied {
            Some(range) => (Some(range.start + 1), Some(range.end + 1), range.confidence),
            None => (None, None, "unmatched".to_string()),
        };

        mapped_edits.push(MappingEdit {
            id: edit.id.clone(),
            tool: edit.tool,
            file_path: normalized_path,
            start_line,
            end_line,
            message_id: edit.message_id.clone(),
            user_message_id: edit.user_message_id.clone(),
            confidence,
        });
    }

    let mut edit_hunks = Vec::new();
    let mut linked_edits: HashMap<String, usize> = HashMap::new();
    for edit in &mapped_edits {
        let Some(start) = edit.start_line else {
            continue;
        };
        let end = edit.end_line.unwrap_or(start);
        for hunk in &hunks {
            if hunk.file_path != edit.file_path {
                continue;
            }
            let new_start = hunk.new_start;
            let new_end = if hunk.new_lines == 0 {
                new_start
            } else {
                new_start + hunk.new_lines.saturating_sub(1)
            };
            if ranges_overlap(start, end, new_start, new_end) {
                edit_hunks.push(EditHunkLink {
                    edit_id: edit.id.clone(),
                    hunk_id: hunk.id.clone(),
                });
                linked_edits.insert(edit.id.clone(), 1);
            }
        }
    }

    if !hunks.is_empty() {
        let mut hunks_by_file: HashMap<String, Vec<&DiffHunk>> = HashMap::new();
        for hunk in &hunks {
            hunks_by_file
                .entry(hunk.file_path.clone())
                .or_default()
                .push(hunk);
        }
        for edit in &mapped_edits {
            if linked_edits.contains_key(&edit.id) {
                continue;
            }
            let file_hunks = hunks_by_file.get(&edit.file_path);
            let Some(file_hunks) = file_hunks else {
                continue;
            };
            if file_hunks.is_empty() {
                continue;
            }
            let chosen = if file_hunks.len() == 1 {
                Some(file_hunks[0])
            } else if let Some(start) = edit.start_line {
                let mut best = None;
                let mut best_dist = usize::MAX;
                for hunk in file_hunks {
                    let dist = if hunk.new_start > start {
                        hunk.new_start - start
                    } else {
                        start - hunk.new_start
                    };
                    if dist < best_dist {
                        best_dist = dist;
                        best = Some(*hunk);
                    }
                }
                best
            } else {
                None
            };
            if let Some(hunk) = chosen {
                edit_hunks.push(EditHunkLink {
                    edit_id: edit.id.clone(),
                    hunk_id: hunk.id.clone(),
                });
            }
        }
    }

    Ok(MappingResult {
        base: options.base,
        head: options.head,
        messages,
        edits: mapped_edits,
        hunks,
        edit_hunks,
        errors,
    })
}

fn ranges_overlap(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    a_start <= b_end && b_start <= a_end
}

fn detect_tool(path: &Path) -> Result<TranscriptTool> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(50) {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|v| v.as_str()) == Some("session_meta") {
            let originator = value
                .pointer("/payload/originator")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if originator.starts_with("codex_") {
                return Ok(TranscriptTool::Codex);
            }
        }
        if value.get("type").and_then(|v| v.as_str()) == Some("user") {
            return Ok(TranscriptTool::Claude);
        }
        if value.get("sessionId").is_some() && value.get("message").is_some() {
            return Ok(TranscriptTool::Claude);
        }
    }
    Ok(TranscriptTool::Unknown)
}

fn parse_codex_transcript(
    path: &Path,
    order: &mut usize,
) -> Result<(Vec<MappingMessage>, Vec<RawEdit>)> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    let mut edits = Vec::new();
    let mut last_user_id: Option<String> = None;
    let mut last_assistant_id: Option<String> = None;

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
        let typ = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if typ != "response_item" {
            continue;
        }
        let payload = value.get("payload").unwrap_or(&Value::Null);
        let payload_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match payload_type {
            "message" => {
                let role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
                let content = extract_text(payload.get("content"));
                let msg_id = format!("codex:{}", messages.len());
                let timestamp = value
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                messages.push(MappingMessage {
                    id: msg_id.clone(),
                    role: role.to_string(),
                    content,
                    timestamp,
                    parent_id: None,
                    tool: TranscriptTool::Codex,
                });
                if role == "user" {
                    last_user_id = Some(msg_id);
                } else if role == "assistant" {
                    last_assistant_id = Some(msg_id);
                }
            }
            "custom_tool_call" => {
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if name != "apply_patch" {
                    continue;
                }
                let input = payload.get("input").and_then(|v| v.as_str()).unwrap_or("");
                let patch_files = parse_patch(input);
                let timestamp = value
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if patch_files.is_empty() {
                    let edit_id = format!("edit:{}", edits.len());
                    edits.push(RawEdit {
                        id: edit_id,
                        tool: TranscriptTool::Codex,
                        file_path: "unknown".to_string(),
                    patch: Some(input.to_string()),
                    old_string: None,
                    new_string: None,
                        message_id: last_assistant_id.clone(),
                        user_message_id: last_user_id.clone(),
                        timestamp: timestamp.clone(),
                        order: next_order(order),
                    });
                } else {
                    for patch_file in patch_files {
                        let edit_id = format!("edit:{}", edits.len());
                        edits.push(RawEdit {
                            id: edit_id,
                            tool: TranscriptTool::Codex,
                            file_path: patch_file.path,
                            patch: Some(input.to_string()),
                            old_string: None,
                            new_string: None,
                            message_id: last_assistant_id.clone(),
                            user_message_id: last_user_id.clone(),
                            timestamp: timestamp.clone(),
                            order: next_order(order),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    Ok((messages, edits))
}

fn parse_claude_transcript(
    path: &Path,
    order: &mut usize,
) -> Result<(Vec<MappingMessage>, Vec<RawEdit>)> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    let mut edits = Vec::new();

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
        let message = value.get("message").unwrap_or(&Value::Null);
        if message.is_null() {
            continue;
        }
        let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role.is_empty() {
            continue;
        }
        let content = extract_text(message.get("content"));
        let msg_id = value
            .get("uuid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let timestamp = value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let parent_id = value
            .get("parentUuid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if !msg_id.is_empty() {
            messages.push(MappingMessage {
                id: msg_id.clone(),
                role: role.to_string(),
                content,
                timestamp: timestamp.clone(),
                parent_id: parent_id.clone(),
                tool: TranscriptTool::Claude,
            });
        }

        if role != "assistant" {
            continue;
        }
        let Some(items) = message.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        for item in items {
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if item_type != "tool_use" {
                continue;
            }
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name != "Edit" {
                continue;
            }
            let input = item.get("input").unwrap_or(&Value::Null);
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let old_string = input
                .get("old_string")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let new_string = input
                .get("new_string")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if file_path.is_empty() || old_string.is_none() {
                continue;
            }
            let edit_id = format!("edit:{}", edits.len());
            edits.push(RawEdit {
                id: edit_id,
                tool: TranscriptTool::Claude,
                file_path,
                patch: None,
                old_string,
                new_string,
                message_id: if msg_id.is_empty() { None } else { Some(msg_id.clone()) },
                user_message_id: parent_id.clone(),
                timestamp: timestamp.clone(),
                order: next_order(order),
            });
        }
    }

    let mut message_index: HashMap<String, (String, Option<String>)> = HashMap::new();
    for message in &messages {
        message_index.insert(message.id.clone(), (message.role.clone(), message.parent_id.clone()));
    }
    for edit in &mut edits {
        let Some(message_id) = edit.message_id.clone() else {
            continue;
        };
        if let Some(user_id) = resolve_user_ancestor(&message_id, &message_index) {
            edit.user_message_id = Some(user_id);
        }
    }

    Ok((messages, edits))
}

fn next_order(order: &mut usize) -> usize {
    let current = *order;
    *order += 1;
    current
}

fn extract_text(value: Option<&Value>) -> String {
    let Some(value) = value else { return String::new() };
    match value {
        Value::String(text) => text.to_string(),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                if let Some(text) = extract_text_from_item(item) {
                    if !text.trim().is_empty() {
                        parts.push(text);
                    }
                }
            }
            parts.join("\n")
        }
        Value::Object(map) => map
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

fn extract_text_from_item(item: &Value) -> Option<String> {
    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
        return Some(text.to_string());
    }
    if let Some(content) = item.get("content") {
        return Some(extract_text(Some(content)));
    }
    if let Some(value) = item.get("value") {
        return Some(extract_text(Some(value)));
    }
    None
}

fn resolve_user_ancestor(
    message_id: &str,
    message_index: &HashMap<String, (String, Option<String>)>,
) -> Option<String> {
    let mut current = Some(message_id.to_string());
    let mut steps = 0usize;
    while let Some(id) = current {
        if steps > 64 {
            break;
        }
        let Some((role, parent)) = message_index.get(&id) else {
            break;
        };
        if role == "user" {
            return Some(id);
        }
        current = parent.clone();
        steps += 1;
    }
    None
}

fn load_base_file(repo: &Path, base: &str, file_path: &str) -> FileState {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("show")
        .arg(format!("{base}:{file_path}"))
        .output();
    let content = match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        _ => String::new(),
    };
    FileState {
        lines: content.lines().map(|s| s.to_string()).collect(),
    }
}

fn normalize_file_path(repo: &Path, path: &str) -> String {
    let repo_real = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let repo_real = repo_real.to_string_lossy();
    if path.starts_with(repo_real.as_ref()) {
        return path[repo_real.len()..].trim_start_matches('/').to_string();
    }
    let repo_raw = repo.to_string_lossy();
    if path.starts_with(repo_raw.as_ref()) {
        return path[repo_raw.len()..].trim_start_matches('/').to_string();
    }
    path.to_string()
}

fn apply_claude_edit(
    state: &mut FileState,
    old_string: Option<&str>,
    new_string: Option<&str>,
) -> Option<AppliedRange> {
    let Some(old_string) = old_string else { return None };
    let new_string = new_string.unwrap_or("");

    let mut content = state.lines.join("\n");
    let normalized_old = old_string.replace("\r\n", "\n");
    let normalized_new = new_string.replace("\r\n", "\n");
    let index = content.find(&normalized_old)?;
    let start_line = content[..index].matches('\n').count();
    let old_lines = normalized_old.matches('\n').count();
    content.replace_range(index..index + normalized_old.len(), &normalized_new);
    let new_lines = normalized_new.matches('\n').count();
    state.lines = content.lines().map(|s| s.to_string()).collect();

    let end_line = if new_lines > 0 {
        start_line + new_lines
    } else if old_lines > 0 {
        start_line
    } else {
        start_line
    };

    Some(AppliedRange {
        start: start_line,
        end: end_line,
        confidence: "exact".to_string(),
    })
}

fn apply_codex_patch(state: &mut FileState, patch: &str, file_path: &str) -> Option<AppliedRange> {
    let files = parse_patch(patch);
    let mut best: Option<AppliedRange> = None;
    for file in files {
        if file.path != file_path {
            continue;
        }
        if file.hunks.is_empty() {
            continue;
        }
        for hunk in &file.hunks {
            if let Some(range) = apply_hunk(&mut state.lines, hunk) {
                best = Some(range);
            }
        }
    }
    best
}

fn parse_patch(patch: &str) -> Vec<PatchFile> {
    let mut files = Vec::new();
    let mut current: Option<PatchFile> = None;
    let mut current_hunk: Option<PatchHunk> = None;

    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            flush_hunk(&mut current, &mut current_hunk);
            flush_file(&mut files, &mut current);
            current = Some(PatchFile {
                path: path.trim().to_string(),
                hunks: Vec::new(),
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            flush_hunk(&mut current, &mut current_hunk);
            flush_file(&mut files, &mut current);
            current = Some(PatchFile {
                path: path.trim().to_string(),
                hunks: Vec::new(),
            });
            continue;
        }
        if line.starts_with("@@") {
            flush_hunk(&mut current, &mut current_hunk);
            current_hunk = Some(PatchHunk { lines: Vec::new() });
            continue;
        }
        if let Some(hunk) = current_hunk.as_mut() {
            if let Some((kind, text)) = line.split_once(' ') {
                let kind_char = kind.chars().next().unwrap_or(' ');
                if kind_char == '+' || kind_char == '-' || kind_char == ' ' {
                    hunk.lines.push(HunkLine {
                        kind: kind_char,
                        text: text.to_string(),
                    });
                }
            } else if let Some(kind_char) = line.chars().next() {
                if kind_char == '+' || kind_char == '-' || kind_char == ' ' {
                    hunk.lines.push(HunkLine {
                        kind: kind_char,
                        text: line[1..].to_string(),
                    });
                }
            }
        }
    }
    flush_hunk(&mut current, &mut current_hunk);
    flush_file(&mut files, &mut current);
    files
}

fn flush_hunk(current: &mut Option<PatchFile>, hunk: &mut Option<PatchHunk>) {
    if let (Some(file), Some(h)) = (current.as_mut(), hunk.take()) {
        if !h.lines.is_empty() {
            file.hunks.push(h);
        }
    }
}

fn flush_file(files: &mut Vec<PatchFile>, current: &mut Option<PatchFile>) {
    if let Some(file) = current.take() {
        files.push(file);
    }
}

fn apply_hunk(lines: &mut Vec<String>, hunk: &PatchHunk) -> Option<AppliedRange> {
    let before: Vec<String> = hunk
        .lines
        .iter()
        .filter_map(|line| match line.kind {
            '+' => None,
            _ => Some(line.text.clone()),
        })
        .collect();

    if let Some(pos) = find_subslice(lines, &before) {
        let (new_lines, start, end) = apply_at(lines, hunk, pos)?;
        *lines = new_lines;
        return Some(AppliedRange {
            start,
            end,
            confidence: "exact".to_string(),
        });
    }

    let context: Vec<String> = hunk
        .lines
        .iter()
        .filter_map(|line| match line.kind {
            ' ' => Some(line.text.clone()),
            _ => None,
        })
        .collect();
    if let Some(pos) = find_subslice(lines, &context) {
        let (new_lines, start, end) = apply_at(lines, hunk, pos)?;
        *lines = new_lines;
        return Some(AppliedRange {
            start,
            end,
            confidence: "context".to_string(),
        });
    }

    None
}

fn apply_at(lines: &[String], hunk: &PatchHunk, pos: usize) -> Option<(Vec<String>, usize, usize)> {
    let mut out = Vec::new();
    out.extend_from_slice(&lines[..pos]);
    let mut idx = pos;
    let start = out.len();
    for line in &hunk.lines {
        match line.kind {
            ' ' => {
                if lines.get(idx)? != &line.text {
                    return None;
                }
                out.push(lines[idx].clone());
                idx += 1;
            }
            '-' => {
                if lines.get(idx)? != &line.text {
                    return None;
                }
                idx += 1;
            }
            '+' => {
                out.push(line.text.clone());
            }
            _ => {}
        }
    }
    let end = if out.len() == start {
        start
    } else {
        out.len() - 1
    };
    out.extend_from_slice(&lines[idx..]);
    Some((out, start, end))
}

fn find_subslice(haystack: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    for i in 0..=haystack.len() - needle.len() {
        if haystack[i..i + needle.len()] == needle[..] {
            return Some(i);
        }
    }
    None
}

fn git_diff_hunks(repo: &Path, base: &str, head: &str) -> Result<Vec<DiffHunk>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("diff")
        .arg("--unified=0")
        .arg(base)
        .arg(head)
        .output()
        .context("failed to run git diff")?;
    if !output.status.success() {
        return Err(anyhow::anyhow!("git diff failed"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_unified_diff(&text))
}

fn parse_unified_diff(diff: &str) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut current_path: Option<String> = None;

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_path = Some(path.to_string());
            continue;
        }
        if !line.starts_with("@@") {
            continue;
        }
        let Some(path) = current_path.clone() else {
            continue;
        };
        let (old_start, old_lines, new_start, new_lines) = parse_hunk_header(line);
        let id = format!("hunk:{}", hunks.len());
        hunks.push(DiffHunk {
            id,
            file_path: path,
            old_start,
            old_lines,
            new_start,
            new_lines,
        });
    }
    hunks
}

fn parse_hunk_header(header: &str) -> (usize, usize, usize, usize) {
    let parts: Vec<&str> = header.split_whitespace().collect();
    if parts.len() < 3 {
        return (0, 0, 0, 0);
    }
    let old = parts[1].trim_start_matches('-');
    let new = parts[2].trim_start_matches('+');
    let (old_start, old_lines) = parse_range(old);
    let (new_start, new_lines) = parse_range(new);
    (old_start, old_lines, new_start, new_lines)
}

fn parse_range(text: &str) -> (usize, usize) {
    let mut iter = text.split(',');
    let start = iter.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
    let lines = iter.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(1);
    (start, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_patch_exact_match() {
        let mut lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let hunk = PatchHunk {
            lines: vec![
                HunkLine {
                    kind: ' ',
                    text: "b".to_string(),
                },
                HunkLine {
                    kind: '-',
                    text: "c".to_string(),
                },
                HunkLine {
                    kind: '+',
                    text: "d".to_string(),
                },
            ],
        };
        let range = apply_hunk(&mut lines, &hunk).unwrap();
        assert_eq!(lines, vec!["a", "b", "d"]);
        assert_eq!(range.start, 1);
        assert_eq!(range.end, 2);
    }

    #[test]
    fn apply_edit_replaces_string() {
        let mut state = FileState {
            lines: vec!["hello".to_string(), "world".to_string()],
        };
        let range = apply_claude_edit(&mut state, Some("world"), Some("codex")).unwrap();
        assert_eq!(state.lines, vec!["hello", "codex"]);
        assert_eq!(range.start, 1);
        assert_eq!(range.end, 1);
    }

    #[test]
    fn parse_diff_hunk_header() {
        let (old_start, old_lines, new_start, new_lines) = parse_hunk_header("@@ -10,2 +20,5 @@");
        assert_eq!((old_start, old_lines, new_start, new_lines), (10, 2, 20, 5));
    }
}
