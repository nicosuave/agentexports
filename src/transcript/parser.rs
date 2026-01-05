//! Transcript parsing: JSONL format parsing for Claude and Codex transcripts.

use anyhow::Result;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use super::types::{MessageUsage, ParseResult, RenderedMessage, TranscriptMeta};

/// Truncate a string to max_chars, adding "..." if truncated
pub fn truncate(input: &str, max_chars: usize) -> String {
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

/// Check if text looks like an internal/system block that should be filtered
pub fn looks_like_internal_block(text: &str) -> bool {
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

/// Normalize role names to standard values
pub fn normalize_role(role: &str) -> String {
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

/// Recursively extract text from a JSON value
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

/// Extract transcript metadata (title, first user message)
pub fn extract_transcript_meta(path: &Path) -> TranscriptMeta {
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
        if meta.slug.is_some() && meta.first_user_message.is_some() {
            break;
        }
    }

    meta
}

/// Parse a transcript file into messages and metadata
pub fn parse_transcript(path: &Path) -> Result<ParseResult> {
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
        if matches!(event_type, "file-history-snapshot" | "queue-operation") {
            continue;
        }

        // Claude: render compaction summary as a system message in natural order
        if event_type == "summary" {
            if let Some(summary) = value.get("summary").and_then(|v| v.as_str()) {
                result.messages.push(RenderedMessage {
                    role: "system".to_string(),
                    content: format!("**Session Summary:** {}", summary),
                    raw: None,
                    raw_label: None,
                    tool_use_id: None,
                    model: None,
                });
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ===== truncate tests =====

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long() {
        assert_eq!(truncate("hello world", 5), "hello...");
    }

    #[test]
    fn test_truncate_unicode() {
        // Truncate by character count, not bytes
        assert_eq!(truncate("日本語テスト", 3), "日本語...");
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate("", 5), "");
    }

    #[test]
    fn test_truncate_zero_limit() {
        assert_eq!(truncate("hello", 0), "...");
    }

    // ===== looks_like_internal_block tests =====

    #[test]
    fn test_looks_like_internal_block_env_context() {
        assert!(looks_like_internal_block(
            "<environment_context>\n  <cwd>/tmp</cwd>\n</environment_context>"
        ));
    }

    #[test]
    fn test_looks_like_internal_block_env_context_with_whitespace() {
        assert!(looks_like_internal_block(
            "  <environment_context>\n  <cwd>/tmp</cwd>\n</environment_context>"
        ));
    }

    #[test]
    fn test_looks_like_internal_block_instructions() {
        assert!(looks_like_internal_block(
            "<INSTRUCTIONS>\nDo something\n</INSTRUCTIONS>"
        ));
    }

    #[test]
    fn test_looks_like_internal_block_agents_md() {
        assert!(looks_like_internal_block("# AGENTS.md\nThis is agents config"));
    }

    #[test]
    fn test_looks_like_internal_block_embedded_env() {
        assert!(looks_like_internal_block(
            "Some prefix\n<environment_context>\n</environment_context>"
        ));
    }

    #[test]
    fn test_looks_like_internal_block_embedded_instructions() {
        assert!(looks_like_internal_block(
            "Some prefix\n<INSTRUCTIONS>\n</INSTRUCTIONS>"
        ));
    }

    #[test]
    fn test_looks_like_internal_block_normal_text() {
        assert!(!looks_like_internal_block("Hello, how can I help you?"));
    }

    #[test]
    fn test_looks_like_internal_block_code() {
        assert!(!looks_like_internal_block("fn main() { println!(\"hello\"); }"));
    }

    // ===== normalize_role tests =====

    #[test]
    fn test_normalize_role_assistant() {
        assert_eq!(normalize_role("assistant"), "assistant");
        assert_eq!(normalize_role("ASSISTANT"), "assistant");
        assert_eq!(normalize_role("Assistant"), "assistant");
    }

    #[test]
    fn test_normalize_role_model() {
        assert_eq!(normalize_role("model"), "assistant");
    }

    #[test]
    fn test_normalize_role_user() {
        assert_eq!(normalize_role("user"), "user");
        assert_eq!(normalize_role("USER"), "user");
        assert_eq!(normalize_role("User"), "user");
    }

    #[test]
    fn test_normalize_role_human() {
        assert_eq!(normalize_role("human"), "user");
        assert_eq!(normalize_role("Human"), "user");
    }

    #[test]
    fn test_normalize_role_system() {
        assert_eq!(normalize_role("system"), "system");
        assert_eq!(normalize_role("SYSTEM"), "system");
    }

    #[test]
    fn test_normalize_role_tool() {
        assert_eq!(normalize_role("tool"), "tool");
        assert_eq!(normalize_role("function"), "tool");
    }

    #[test]
    fn test_normalize_role_unknown() {
        assert_eq!(normalize_role("custom"), "custom");
        assert_eq!(normalize_role("UNKNOWN"), "unknown");
    }

    #[test]
    fn test_normalize_role_with_whitespace() {
        assert_eq!(normalize_role("  user  "), "user");
    }

    // ===== parse_transcript tests =====

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
