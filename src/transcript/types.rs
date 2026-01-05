//! Types for transcript parsing and rendering.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Which tool produced the transcript
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

    pub fn display_name(self) -> &'static str {
        match self {
            Tool::Claude => "Claude Code",
            Tool::Codex => "Codex",
        }
    }
}

/// A rendered message for the share payload
#[derive(Debug, Clone, Serialize)]
pub struct RenderedMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Metadata extracted from the transcript (title, first message, etc.)
#[derive(Debug, Clone, Default)]
pub struct TranscriptMeta {
    pub slug: Option<String>,
    pub first_user_message: Option<String>,
}

/// Token usage for a single message
#[derive(Debug, Clone, Default)]
pub struct MessageUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

/// Result of parsing a transcript
#[derive(Debug, Default)]
pub struct ParseResult {
    pub messages: Vec<RenderedMessage>,
    /// Model usage counts for determining dominant model
    pub model_counts: HashMap<String, usize>,
    /// Token usage by message ID (deduplicated - later values overwrite earlier)
    pub usage_by_message_id: HashMap<String, MessageUsage>,
    /// Token usage totals (for Codex cumulative totals, not deduplicated)
    pub codex_total_input_tokens: u64,
    pub codex_total_output_tokens: u64,
    pub codex_total_cache_read_tokens: u64,
}

impl ParseResult {
    /// Get sorted list of models by usage (most used first)
    pub fn models_by_usage(&self) -> Vec<String> {
        let mut models: Vec<_> = self.model_counts.iter().collect();
        models.sort_by(|a, b| b.1.cmp(a.1));
        models.into_iter().map(|(k, _)| k.clone()).collect()
    }

    /// Get the dominant (most used) model
    pub fn dominant_model(&self) -> Option<String> {
        self.models_by_usage().into_iter().next()
    }

    /// Compute total input tokens (Claude: sum deduplicated, Codex: use cumulative)
    pub fn total_input_tokens(&self) -> u64 {
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
    pub fn total_output_tokens(&self) -> u64 {
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
    pub fn total_cache_read_tokens(&self) -> u64 {
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
    pub fn total_cache_creation_tokens(&self) -> u64 {
        self.usage_by_message_id
            .values()
            .map(|u| u.cache_creation_tokens)
            .sum()
    }
}

fn is_zero(val: &u64) -> bool {
    *val == 0
}

/// Payload sent to the viewer (encrypted JSON)
#[derive(Debug, Clone, Serialize)]
pub struct SharePayload {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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
