//! Gist rendering: convert SharePayload to GitHub gist markdown.

use anyhow::{Context, Result};

/// Render payload JSON into a markdown document for GitHub Gist
pub fn render_gist_markdown(payload_json: &str) -> Result<String> {
    let payload: serde_json::Value =
        serde_json::from_str(payload_json).context("Failed to parse payload JSON")?;

    let mut md = String::new();

    // Title
    let title = payload
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Agent Export");
    md.push_str(&format!("# {}\n\n", title));

    // Metadata
    let tool = payload.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let model = payload.get("model").and_then(|v| v.as_str());
    let models = payload.get("models").and_then(|v| v.as_array());
    let shared_at = payload
        .get("shared_at")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let model_str = if let Some(m) = model {
        m.to_string()
    } else if let Some(ms) = models {
        ms.iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(" + ")
    } else {
        String::new()
    };

    if !tool.is_empty() || !model_str.is_empty() || !shared_at.is_empty() {
        let mut meta_parts = Vec::new();
        if !tool.is_empty() {
            meta_parts.push(tool.to_string());
        }
        if !model_str.is_empty() {
            meta_parts.push(model_str);
        }
        if !shared_at.is_empty() {
            meta_parts.push(shared_at.to_string());
        }
        md.push_str(&format!("*{}*\n\n", meta_parts.join(" · ")));
    }

    md.push_str("---\n\n");

    // Messages
    if let Some(messages) = payload.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            let role = msg
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("assistant");
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let msg_model = msg.get("model").and_then(|v| v.as_str());

            // Role header
            let role_display = match role {
                "user" => "User",
                "assistant" => "Assistant",
                "tool" => "Tool",
                "thinking" => "Thinking",
                "system" => "System",
                _ => role,
            };

            let model_suffix = msg_model.map(|m| format!(" ({})", m)).unwrap_or_default();
            md.push_str(&format!("### {}{}\n\n", role_display, model_suffix));

            // Content - for tool messages, wrap in code block if not already
            if role == "tool" && !content.trim().starts_with("```") {
                // Check if it looks like JSON or code
                let trimmed = content.trim();
                if trimmed.starts_with('{') || trimmed.starts_with('[') || trimmed.contains('\n') {
                    md.push_str("```\n");
                    md.push_str(content);
                    if !content.ends_with('\n') {
                        md.push('\n');
                    }
                    md.push_str("```\n\n");
                } else {
                    md.push_str(&format!("`{}`\n\n", content));
                }
            } else {
                md.push_str(content);
                if !content.ends_with('\n') {
                    md.push('\n');
                }
                md.push('\n');
            }

            // Raw/details section (collapsed)
            if let Some(raw) = msg.get("raw").and_then(|v| v.as_str()) {
                let label = msg
                    .get("raw_label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Details");
                md.push_str(&format!(
                    "<details>\n<summary>{}</summary>\n\n```json\n{}\n```\n\n</details>\n\n",
                    label, raw
                ));
            }
        }
    }

    // Token stats
    let input_tokens = payload
        .get("total_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = payload
        .get("total_output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_read = payload
        .get("total_cache_read_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_write = payload
        .get("total_cache_creation_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if input_tokens > 0 || output_tokens > 0 {
        md.push_str("---\n\n");
        let mut stats = Vec::new();
        if input_tokens > 0 {
            stats.push(format!("Input: {} tokens", input_tokens));
        }
        if output_tokens > 0 {
            stats.push(format!("Output: {} tokens", output_tokens));
        }
        if cache_read > 0 {
            stats.push(format!("Cache read: {} tokens", cache_read));
        }
        if cache_write > 0 {
            stats.push(format!("Cache write: {} tokens", cache_write));
        }
        md.push_str(&format!("*{}*\n", stats.join(" · ")));
    }

    Ok(md)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_gist_markdown_basic() {
        let payload = serde_json::json!({
            "title": "Test Session",
            "tool": "Claude Code",
            "shared_at": "Jan 4, 2025 10:30am",
            "messages": [
                {"role": "user", "content": "Hello, world!"},
                {"role": "assistant", "content": "Hi there!"}
            ]
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        assert!(md.contains("# Test Session"));
        assert!(md.contains("Claude Code"));
        assert!(md.contains("Jan 4, 2025 10:30am"));
        assert!(md.contains("### User"));
        assert!(md.contains("Hello, world!"));
        assert!(md.contains("### Assistant"));
        assert!(md.contains("Hi there!"));
    }

    #[test]
    fn test_render_gist_markdown_all_roles() {
        let payload = serde_json::json!({
            "title": "Multi-role Test",
            "messages": [
                {"role": "user", "content": "User message"},
                {"role": "assistant", "content": "Assistant message"},
                {"role": "tool", "content": "Tool output"},
                {"role": "thinking", "content": "Thinking about it..."},
                {"role": "system", "content": "System instruction"}
            ]
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        assert!(md.contains("### User"));
        assert!(md.contains("### Assistant"));
        assert!(md.contains("### Tool"));
        assert!(md.contains("### Thinking"));
        assert!(md.contains("### System"));
    }

    #[test]
    fn test_render_gist_markdown_tool_code_blocks() {
        // Tool messages with JSON should be wrapped in code blocks
        let payload = serde_json::json!({
            "title": "Tool Test",
            "messages": [
                {"role": "tool", "content": "{\"result\": \"success\"}"}
            ]
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        assert!(md.contains("```\n{\"result\": \"success\"}\n```"));
    }

    #[test]
    fn test_render_gist_markdown_tool_multiline_code_blocks() {
        // Tool messages with multiline content should be wrapped in code blocks
        let payload = serde_json::json!({
            "title": "Tool Test",
            "messages": [
                {"role": "tool", "content": "line1\nline2\nline3"}
            ]
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        assert!(md.contains("```\nline1\nline2\nline3\n```"));
    }

    #[test]
    fn test_render_gist_markdown_tool_simple_inline() {
        // Simple tool output without JSON/multiline should be inline code
        let payload = serde_json::json!({
            "title": "Tool Test",
            "messages": [
                {"role": "tool", "content": "success"}
            ]
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        assert!(md.contains("`success`"));
    }

    #[test]
    fn test_render_gist_markdown_with_raw_details() {
        let payload = serde_json::json!({
            "title": "Details Test",
            "messages": [
                {
                    "role": "tool",
                    "content": "Tool result",
                    "raw": "{\"detailed\": \"output\"}",
                    "raw_label": "Full Output"
                }
            ]
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        assert!(md.contains("<details>"));
        assert!(md.contains("<summary>Full Output</summary>"));
        assert!(md.contains("```json"));
        assert!(md.contains("{\"detailed\": \"output\"}"));
        assert!(md.contains("</details>"));
    }

    #[test]
    fn test_render_gist_markdown_token_stats() {
        let payload = serde_json::json!({
            "title": "Token Test",
            "messages": [],
            "total_input_tokens": 1000,
            "total_output_tokens": 500,
            "total_cache_read_tokens": 200,
            "total_cache_creation_tokens": 100
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        assert!(md.contains("Input: 1000 tokens"));
        assert!(md.contains("Output: 500 tokens"));
        assert!(md.contains("Cache read: 200 tokens"));
        assert!(md.contains("Cache write: 100 tokens"));
    }

    #[test]
    fn test_render_gist_markdown_no_stats_when_zero() {
        let payload = serde_json::json!({
            "title": "No Stats",
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        // Should not have the stats footer separator when no tokens
        let parts: Vec<&str> = md.split("---").collect();
        // First separator is after metadata, should only have that one
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn test_render_gist_markdown_multiple_models() {
        let payload = serde_json::json!({
            "title": "Multi-model",
            "models": ["claude-sonnet-4", "claude-haiku"],
            "messages": []
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        assert!(md.contains("claude-sonnet-4 + claude-haiku"));
    }

    #[test]
    fn test_render_gist_markdown_single_model() {
        let payload = serde_json::json!({
            "title": "Single model",
            "model": "claude-opus-4",
            "messages": []
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        assert!(md.contains("claude-opus-4"));
    }

    #[test]
    fn test_render_gist_markdown_message_model_suffix() {
        let payload = serde_json::json!({
            "title": "Model per message",
            "messages": [
                {"role": "assistant", "content": "Hello", "model": "claude-sonnet-4"}
            ]
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        assert!(md.contains("### Assistant (claude-sonnet-4)"));
    }

    #[test]
    fn test_render_gist_markdown_missing_title() {
        let payload = serde_json::json!({
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let md = render_gist_markdown(&payload.to_string()).unwrap();

        // Uses default title
        assert!(md.contains("# Agent Export"));
    }
}
