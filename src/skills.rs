use anyhow::{Context, Result, bail};
use dialoguer::{MultiSelect, theme::ColorfulTheme};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};

use crate::Tool;

const CLAUDE_SKILL_SRC: &str = "skills/claude/agentexport/SKILL.md";
const CLAUDE_HOOK_SRC: &str = "skills/claude/hooks/agentexport";
const CODEX_PROMPT_SRC: &str = "skills/codex/agentexport.md";
const CLAUDE_HOOK_NAME: &str = "agentexport";

pub fn setup_skills_interactive() -> Result<()> {
    let theme = ColorfulTheme::default();

    // Detect installed tools
    let claude_path = find_in_path("claude");
    let codex_path = find_in_path("codex");

    if claude_path.is_none() && codex_path.is_none() {
        bail!("Neither claude nor codex found in PATH");
    }

    // Show what will be installed
    println!("This will install:");
    if claude_path.is_some() {
        println!("  Claude Code: /agentexport skill + SessionStart hook");
    }
    if codex_path.is_some() {
        println!("  Codex: /agentexport prompt");
    }
    println!();

    // Build selection list (only installed tools)
    let mut items: Vec<(Tool, String)> = Vec::new();
    let mut defaults = Vec::new();

    if let Some(path) = &claude_path {
        items.push((Tool::Claude, format!("Claude Code ({})", path.display())));
        defaults.push(true);
    }
    if let Some(path) = &codex_path {
        items.push((Tool::Codex, format!("Codex ({})", path.display())));
        defaults.push(true);
    }

    let labels: Vec<&str> = items.iter().map(|(_, label)| label.as_str()).collect();

    let selected = MultiSelect::with_theme(&theme)
        .with_prompt("Select tools to configure")
        .items(&labels)
        .defaults(&defaults)
        .interact()?;

    if selected.is_empty() {
        println!("Nothing selected.");
        return Ok(());
    }

    println!();

    for index in selected {
        let (tool, _) = &items[index];
        match tool {
            Tool::Claude => {
                install_claude_skill()?;
                install_claude_hook()?;
                ensure_claude_sessionstart_config()?;
            }
            Tool::Codex => {
                install_codex_prompt()?;
            }
        }
    }

    println!();
    println!("Done! Restart Claude Code / Codex to pick up changes.");

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
        println!(
            "Skipping Claude skill (already installed at {}).",
            dest.display()
        );
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
    let dest = dest_dir.join("agentexport.md");
    if dest.exists() {
        println!(
            "Skipping Codex prompt (already installed at {}).",
            dest.display()
        );
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
        println!(
            "Skipping Claude hook (already installed at {}).",
            dest.display()
        );
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
    let hooks_value = root_obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
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
    let hooks_list = entry_obj
        .entry("hooks")
        .or_insert_with(|| Value::Array(Vec::new()));
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
