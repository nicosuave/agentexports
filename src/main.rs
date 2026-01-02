use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::Read;
use std::path::PathBuf;

use agentexport::{
    PublishOptions,
    Tool,
    current_term_key,
    handle_claude_sessionstart,
    publish,
    setup_skills_interactive,
};

#[derive(Parser)]
#[command(name = "agentexport", version, about = "Local agent export helper")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(name = "term-key")]
    TermKey,

    #[command(name = "claude-sessionstart")]
    ClaudeSessionstart,

    #[command(name = "publish")]
    Publish {
        #[arg(long)]
        tool: Tool,
        #[arg(long)]
        term_key: Option<String>,
        #[arg(long)]
        transcript: Option<PathBuf>,
        #[arg(long, default_value_t = 10)]
        max_age_minutes: u64,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        upload_url: Option<String>,
        #[arg(long)]
        render: bool,
    },
    #[command(name = "setup-skills")]
    SetupSkills,
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
        Commands::TermKey => {
            let key = current_term_key()?;
            println!("{key}");
        }
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
            render,
        } => {
            let result = publish(PublishOptions {
                tool,
                term_key,
                transcript,
                max_age_minutes,
                out,
                dry_run,
                upload_url,
                render,
            })?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Commands::SetupSkills => {
            setup_skills_interactive()?;
        }
    }
    Ok(())
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}
