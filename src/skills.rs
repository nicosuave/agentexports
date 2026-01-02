use anyhow::{Context, Result, bail};
use dialoguer::{Confirm, MultiSelect, theme::ColorfulTheme};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};

use crate::Tool;

const CLAUDE_SKILL_SRC: &str = "skills/claude/agentexport/SKILL.md";
const CLAUDE_HOOK_SRC: &str = "skills/claude/hooks/memex";
const CODEX_PROMPT_SRC: &str = "skills/codex/publish_export.md";
const CLAUDE_HOOK_NAME: &str = "memex";

pub fn setup_skills_interactive() -> Result<()> {
    let theme = ColorfulTheme::default();

    let targets = [Tool::Claude, Tool::Codex];
    let mut tool_choices = Vec::new();
    let mut tool_items = Vec::new();
    let mut tool_defaults = Vec::new();
    for tool in targets {
        let binary = find_in_path(tool_binary_name(tool));
        let available = binary.is_some();
        let label = match binary.as_deref().and_then(|path| path.to_str()) {
            Some(path) => format!("{} (found at {})", tool_label(tool), path),
            None => format!("{} (not found in PATH)", tool_label(tool)),
        };
        tool_choices.push((tool, available));
        tool_items.push(label);
        tool_defaults.push(available);
    }

    if tool_choices.iter().all(|(_, available)| !*available) {
        bail!("neither claude nor codex binaries found in PATH");
    }

    let proceed = Confirm::with_theme(&theme)
        .with_prompt(
            "This will install user skills/prompts from this binary. If Claude is selected, it will install ~/.claude/hooks/memex and update ~/.claude/settings.json to run it on SessionStart. Continue?",
        )
        .default(true)
        .interact()?;
    if !proceed {
        bail!("aborted");
    }

    let selected = MultiSelect::with_theme(&theme)
        .with_prompt("Select tools to set up user skills for")
        .items(&tool_items)
        .defaults(&tool_defaults)
        .interact()?;

    if selected.is_empty() {
        bail!("no tools selected");
    }

    for index in selected {
        let (tool, available) = tool_choices[index];
        if !available {
            println!(
                "Skipping {} because it is not available in PATH.",
                tool_label(tool)
            );
            continue;
        }

        match tool {
            Tool::Claude => {
                install_claude_skill()?;
                install_claude_hook()?;
                ensure_claude_sessionstart_config()?;
                println!("Restart Claude to pick up new skills and hooks.");
            }
            Tool::Codex => {
                install_codex_prompt()?;
                println!("Restart Codex to pick up new prompts.");
            }
        }
    }

    Ok(())
}

fn install_claude_skill() -> Result<()> {
    let source = repo_path(CLAUDE_SKILL_SRC)?;
    if !source.exists() {
        bail!("missing {CLAUDE_SKILL_SRC} in repo");
    }
    let dest_dir = ensure_claude_skills_dir()?.join("agentexport");
    let dest = dest_dir.join("SKILL.md");
    if dest.exists() {
        println!("Skipping Claude skill (already installed at {}).", dest.display());
        return Ok(());
    }
    fs::create_dir_all(&dest_dir)?;
    fs::copy(&source, &dest)?;
    println!("Installed Claude skill to {}.", dest.display());
    Ok(())
}

fn install_codex_prompt() -> Result<()> {
    let source = repo_path(CODEX_PROMPT_SRC)?;
    if !source.exists() {
        bail!("missing {CODEX_PROMPT_SRC} in repo");
    }
    let dest_dir = ensure_codex_prompts_dir()?;
    let dest = dest_dir.join("publish_export.md");
    if dest.exists() {
        println!("Skipping Codex prompt (already installed at {}).", dest.display());
        return Ok(());
    }
    fs::create_dir_all(&dest_dir)?;
    fs::copy(&source, &dest)?;
    println!("Installed Codex prompt to {}.", dest.display());
    Ok(())
}

fn install_claude_hook() -> Result<()> {
    let source = repo_path(CLAUDE_HOOK_SRC)?;
    if !source.exists() {
        bail!("missing {CLAUDE_HOOK_SRC} in repo");
    }
    let hooks_dir = claude_home_dir()?.join("hooks");
    fs::create_dir_all(&hooks_dir)?;
    let dest = hooks_dir.join(CLAUDE_HOOK_NAME);
    if dest.exists() {
        println!("Skipping Claude hook (already installed at {}).", dest.display());
        return Ok(());
    }
    fs::copy(&source, &dest)?;
    set_executable(&dest)?;
    println!("Installed Claude hook to {}.", dest.display());
    Ok(())
}

fn ensure_claude_sessionstart_config() -> Result<()> {
    let settings_path = claude_home_dir()?.join("settings.json");
    let mut root = if settings_path.exists() {
        let raw = fs::read_to_string(&settings_path)?;
        serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| Value::Object(Map::new()))
    } else {
        Value::Object(Map::new())
    };

    let mut changed = false;
    let root_obj = ensure_object(&mut root);
    let hooks_value = root_obj.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    let hooks_obj = ensure_object(hooks_value);
    let session_value = hooks_obj
        .entry("SessionStart")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !session_value.is_array() {
        *session_value = Value::Array(Vec::new());
    }
    let session_arr = session_value.as_array_mut().unwrap();

    let entry_index = find_or_create_session_entry(session_arr);
    let entry_value = &mut session_arr[entry_index];
    let entry_obj = ensure_object(entry_value);
    let hooks_list = entry_obj.entry("hooks").or_insert_with(|| Value::Array(Vec::new()));
    if !hooks_list.is_array() {
        *hooks_list = Value::Array(Vec::new());
    }
    let hooks_arr = hooks_list.as_array_mut().unwrap();

    let command = format!("\"$HOME/.claude/hooks/{CLAUDE_HOOK_NAME}\"");
    let exists = hooks_arr.iter().any(|item| {
        item.get("type").and_then(|v| v.as_str()) == Some("command")
            && item.get("command").and_then(|v| v.as_str()) == Some(command.as_str())
    });
    if !exists {
        hooks_arr.push(json!({
            "type": "command",
            "command": command,
        }));
        changed = true;
    }

    if changed {
        let text = serde_json::to_string_pretty(&root)?;
        fs::write(&settings_path, format!("{text}\n"))?;
        println!("Updated Claude settings at {}.", settings_path.display());
    } else {
        println!("Claude settings already contain SessionStart hook.");
    }

    Ok(())
}

fn ensure_object(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value.as_object_mut().unwrap()
}

fn find_or_create_session_entry(entries: &mut Vec<Value>) -> usize {
    for (idx, entry) in entries.iter().enumerate() {
        if entry.get("matcher").and_then(|v| v.as_str()) == Some("*") {
            return idx;
        }
    }
    entries.push(json!({
        "matcher": "*",
        "hooks": [],
    }));
    entries.len() - 1
}

fn repo_path(relative: &str) -> Result<PathBuf> {
    let root = std::env::current_dir().context("unable to resolve cwd")?;
    Ok(root.join(relative))
}

fn ensure_claude_skills_dir() -> Result<PathBuf> {
    let dir = claude_home_dir()?.join("skills");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn ensure_codex_prompts_dir() -> Result<PathBuf> {
    let dir = crate::codex_home_dir()?.join("prompts");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn claude_home_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".claude"))
}

fn tool_binary_name(tool: Tool) -> &'static str {
    match tool {
        Tool::Claude => "claude",
        Tool::Codex => "codex",
    }
}

fn tool_label(tool: Tool) -> &'static str {
    match tool {
        Tool::Claude => "Claude",
        Tool::Codex => "Codex",
    }
}

fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() && is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn repo_path_joins_cwd() {
        let tmp = TempDir::new().unwrap();
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let path = repo_path("skills/claude/agentexport/SKILL.md").unwrap();
        assert!(path.ends_with("skills/claude/agentexport/SKILL.md"));
        std::env::set_current_dir(cwd).unwrap();
    }
}
