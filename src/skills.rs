use anyhow::{Context, Result, bail};
use dialoguer::{MultiSelect, theme::ColorfulTheme};
use std::fs;
use std::path::{Path, PathBuf};

use crate::Tool;

const CLAUDE_SKILL_SRC: &str = "skills/claude/agentexport/SKILL.md";
const CODEX_PROMPT_SRC: &str = "skills/codex/agentexport.md";

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
        println!("  Claude Code: /agentexport skill");
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

fn repo_path(relative: &str) -> Result<PathBuf> {
    let root = std::env::current_dir().context("unable to resolve cwd")?;
    Ok(root.join(relative))
}

fn ensure_claude_skills_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let dir = PathBuf::from(home).join(".claude").join("skills");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn ensure_codex_prompts_dir() -> Result<PathBuf> {
    let dir = crate::codex_home_dir()?.join("prompts");
    fs::create_dir_all(&dir)?;
    Ok(dir)
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
