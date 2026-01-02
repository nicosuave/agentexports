use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::Read;
use std::path::PathBuf;

use agentexport::{
    Config, PublishOptions, Tool, handle_claude_sessionstart, publish, setup_skills_interactive,
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
        /// Upload URL (default from ~/.agentexport or https://agentexports.com)
        #[arg(long)]
        upload_url: Option<String>,
        /// Skip uploading to server
        #[arg(long)]
        no_upload: bool,
        #[arg(long)]
        render: bool,
        /// TTL for the share: 30, 60, 90, 180, 365, or 0 for forever (default from ~/.agentexport or 30)
        #[arg(long)]
        ttl: Option<u64>,
    },
    #[command(name = "setup-skills")]
    SetupSkills,

    /// Manage shared transcripts
    #[command(name = "shares")]
    Shares {
        #[command(subcommand)]
        action: Option<SharesAction>,
    },

    /// View or modify config (~/.agentexport)
    #[command(name = "config")]
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
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
        /// Key to set (default_ttl, upload_url)
        key: String,
        /// Value to set
        value: String,
    },
    /// Reset config to defaults
    Reset,
}

fn main() {
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
            let effective_upload_url = if no_upload {
                None
            } else {
                Some(upload_url.unwrap_or(config.upload_url))
            };
            let has_upload_url = effective_upload_url.is_some();
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
            })?;

            // When uploading, print just the share URL to stdout (for piping)
            // Otherwise, print full JSON result
            if has_upload_url {
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
        Commands::SetupSkills => {
            setup_skills_interactive()?;
        }
        Commands::Shares { action } => {
            shares_cmd::run(action)?;
        }
        Commands::Config { action } => {
            handle_config(action)?;
        }
    }
    Ok(())
}

fn handle_config(action: Option<ConfigAction>) -> Result<()> {
    match action {
        None | Some(ConfigAction::Show) => {
            let config = Config::load().unwrap_or_default();
            println!("default_ttl = {}", config.default_ttl);
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
