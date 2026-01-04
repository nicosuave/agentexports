use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::Read;
use std::path::PathBuf;

use agentexport::{
    Config,
    PublishOptions,
    StorageType,
    Tool,
    handle_claude_sessionstart,
    publish,
    run_setup,
};

mod shares_cmd;

#[derive(Parser)]
#[command(name = "agentexport", version, about = "Local agent export helper")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Internal: called by Claude hook
    #[command(name = "claude-sessionstart", hide = true)]
    ClaudeSessionstart,

    #[command(name = "publish")]
    Publish {
        #[arg(long)]
        tool: Tool,
        #[arg(long, hide = true)]
        term_key: Option<String>,
        #[arg(long)]
        transcript: Option<PathBuf>,
        #[arg(long, default_value_t = 10)]
        max_age_minutes: u64,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        /// Upload URL (default from ~/.agentexport/config.toml or https://agentexports.com)
        #[arg(long)]
        upload_url: Option<String>,
        /// Skip uploading to server
        #[arg(long)]
        no_upload: bool,
        #[arg(long)]
        render: bool,
        /// TTL for the share: 30, 60, 90, 180, 365, or 0 for forever (default from ~/.agentexport/config.toml or 30)
        #[arg(long)]
        ttl: Option<u64>,
    },
    #[command(name = "setup")]
    Setup,

    /// Manage shared transcripts
    #[command(name = "shares")]
    Shares {
        #[command(subcommand)]
        action: Option<SharesAction>,
    },

    /// View or modify config (~/.agentexport/config.toml)
    #[command(name = "config")]
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },

    /// Update agentexport to the latest version
    #[command(name = "update")]
    Update {
        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum SharesAction {
    /// List all shares
    List,
    /// Delete a share from the server
    Unshare {
        /// Share ID to delete
        id: String,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Show current config
    Show,
    /// Set a config value
    Set {
        /// Key to set (default_ttl, storage_type, upload_url)
        key: String,
        /// Value to set
        value: String,
    },
    /// Reset config to defaults
    Reset,
}

fn main() {
    check_for_update_async();
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::ClaudeSessionstart => {
            let input = read_stdin()?;
            handle_claude_sessionstart(&input)?;
        }
        Commands::Publish {
            tool,
            term_key,
            transcript,
            max_age_minutes,
            out,
            dry_run,
            upload_url,
            no_upload,
            render,
            ttl,
        } => {
            let config = Config::load().unwrap_or_default();
            let effective_ttl = ttl.unwrap_or(config.default_ttl);
            let effective_storage_type = config.storage_type;
            let effective_upload_url = if no_upload {
                None
            } else if effective_storage_type == StorageType::Gist {
                Some("gist".to_string())
            } else {
                Some(upload_url.unwrap_or(config.upload_url))
            };
            let has_upload_target = effective_upload_url.is_some();
            let result = publish(PublishOptions {
                tool,
                term_key,
                transcript,
                max_age_minutes,
                out,
                dry_run,
                upload_url: effective_upload_url,
                render,
                ttl_days: effective_ttl,
                storage_type: effective_storage_type,
            })?;

            // When uploading, print just the share URL to stdout (for piping)
            // Otherwise, print full JSON result
            if has_upload_target {
                if let Some(url) = &result.share_url {
                    println!("{url}");
                } else {
                    // No URL returned (dry-run or error), print JSON for debugging
                    eprintln!("{}", serde_json::to_string_pretty(&result)?);
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
        }
        Commands::Setup => {
            run_setup()?;
        }
        Commands::Shares { action } => {
            shares_cmd::run(action)?;
        }
        Commands::Config { action } => {
            handle_config(action)?;
        }
        Commands::Update { yes } => {
            run_update(yes)?;
        }
    }
    Ok(())
}

fn handle_config(action: Option<ConfigAction>) -> Result<()> {
    match action {
        None | Some(ConfigAction::Show) => {
            let config = Config::load().unwrap_or_default();
            println!("default_ttl = {}", config.default_ttl);
            println!("storage_type = \"{}\"", config.storage_type);
            println!("upload_url = \"{}\"", config.upload_url);
        }
        Some(ConfigAction::Set { key, value }) => {
            let mut config = Config::load().unwrap_or_default();
            match key.as_str() {
                "default_ttl" | "ttl" => {
                    let ttl: u64 = value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid ttl: must be 0, 30, 60, 90, 180, or 365")
                    })?;
                    if !matches!(ttl, 0 | 30 | 60 | 90 | 180 | 365) {
                        anyhow::bail!("invalid ttl: must be 0, 30, 60, 90, 180, or 365");
                    }
                    config.default_ttl = ttl;
                }
                "storage_type" | "storage" => {
                    config.storage_type = StorageType::parse(&value)?;
                }
                "upload_url" | "url" => {
                    config.upload_url = value;
                }
                _ => {
                    anyhow::bail!("unknown config key: {key}");
                }
            }
            let path = config.save()?;
            println!("saved to {}", path.display());
        }
        Some(ConfigAction::Reset) => {
            let config = Config::default();
            let path = config.save()?;
            println!("reset to defaults at {}", path.display());
        }
    }
    Ok(())
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

const REPO: &str = "nicosuave/agentexport";

fn is_homebrew_install() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.to_str()
                .map(|s| s.contains("/Cellar/") || s.contains("/homebrew/"))
        })
        .unwrap_or(false)
}

fn run_update(skip_confirm: bool) -> Result<()> {
    if is_homebrew_install() {
        println!("agentexport was installed via Homebrew.");
        println!("Run 'brew upgrade agentexport' to update.");
        return Ok(());
    }

    let current = env!("CARGO_PKG_VERSION");
    let latest = fetch_latest_version()?;

    if current == latest {
        println!("agentexport is already up to date (v{current})");
        return Ok(());
    }

    println!("Current version: v{current}");
    println!("Latest version:  v{latest}");
    println!();

    if !skip_confirm {
        use dialoguer::{Confirm, theme::ColorfulTheme};
        let confirm = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(format!("Update to v{latest}?"))
            .default(true)
            .interact()?;
        if !confirm {
            println!("Update cancelled.");
            return Ok(());
        }
    }

    let (os, arch) = detect_platform()?;
    let url = format!(
        "https://github.com/{REPO}/releases/download/v{latest}/agentexport-{latest}-{os}-{arch}.tar.gz"
    );

    println!("Downloading {url}...");

    let tmp_dir = tempfile::tempdir()?;
    let archive_path = tmp_dir.path().join("agentexport.tar.gz");

    // Download using curl with retries (assets may not be immediately available after release)
    let mut last_status = None;
    for attempt in 1..=3 {
        let status = std::process::Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&archive_path)
            .arg(&url)
            .status()?;
        if status.success() {
            last_status = Some(status);
            break;
        }
        last_status = Some(status);
        if attempt < 3 {
            println!("Download failed, retrying in {} seconds...", attempt * 2);
            std::thread::sleep(std::time::Duration::from_secs(attempt as u64 * 2));
        }
    }
    if !last_status.map(|s| s.success()).unwrap_or(false) {
        return Err(anyhow::anyhow!(
            "Failed to download release (assets may still be uploading, try again in a minute)"
        ));
    }

    // Extract
    let status = std::process::Command::new("tar")
        .args(["-xzf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(tmp_dir.path())
        .status()?;
    if !status.success() {
        return Err(anyhow::anyhow!("Failed to extract release"));
    }

    let new_binary = tmp_dir.path().join("agentexport");
    if !new_binary.exists() {
        return Err(anyhow::anyhow!("Binary not found in release archive"));
    }

    // Replace current binary
    let current_exe = std::env::current_exe()?;
    let backup = current_exe.with_extension("old");

    // Move current to backup, move new to current
    if backup.exists() {
        std::fs::remove_file(&backup)?;
    }
    std::fs::rename(&current_exe, &backup)?;
    std::fs::copy(&new_binary, &current_exe)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&current_exe, std::fs::Permissions::from_mode(0o755))?;
    }

    // Remove backup
    let _ = std::fs::remove_file(&backup);

    println!("Updated agentexport to v{latest}");
    Ok(())
}

fn fetch_latest_version() -> Result<String> {
    let output = std::process::Command::new("curl")
        .args([
            "-fsSL",
            &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        ])
        .output()?;

    if !output.status.success() {
        return Err(anyhow::anyhow!("Failed to fetch latest version"));
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let tag = json["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No tag_name in release"))?;

    Ok(tag.trim_start_matches('v').to_string())
}

fn detect_platform() -> Result<(&'static str, &'static str)> {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return Err(anyhow::anyhow!("Unsupported OS"));
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        return Err(anyhow::anyhow!("Unsupported architecture"));
    };

    Ok((os, arch))
}

/// Check for updates in the background and print a warning if outdated.
fn check_for_update_async() {
    let is_brew = is_homebrew_install();
    std::thread::spawn(move || {
        if let Ok(latest) = fetch_latest_version() {
            let current = env!("CARGO_PKG_VERSION");
            if current != latest {
                let upgrade_cmd = if is_brew {
                    "brew upgrade agentexport"
                } else {
                    "agentexport update"
                };
                eprintln!(
                    "\x1b[33mA new version of agentexport is available: v{latest} (current: v{current})\x1b[0m"
                );
                eprintln!("\x1b[33mRun '{upgrade_cmd}' to upgrade.\x1b[0m");
            }
        }
    });
}
