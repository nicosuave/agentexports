//! Shares management command implementation.

use anyhow::{Result, bail};
use dialoguer::{Select, theme::ColorfulTheme};
use time::format_description;

use agentexport::{StorageType, shares::{self, Share}};

use crate::SharesAction;

pub fn run(action: Option<SharesAction>) -> Result<()> {
    match action {
        Some(SharesAction::List) => list_shares(),
        Some(SharesAction::Unshare { id }) => unshare(&id),
        None => interactive(),
    }
}

/// List all shares in plain text
fn list_shares() -> Result<()> {
    let shares = shares::load_shares()?;

    if shares.is_empty() {
        println!("No shares found.");
        return Ok(());
    }

    let format = format_description::parse("[year]-[month]-[day] [hour]:[minute]")?;

    for share in shares {
        let status = if share.is_expired() {
            "expired"
        } else {
            "active"
        };
        let created = share.created_at.format(&format).unwrap_or_default();
        println!(
            "{} [{}] {} - {} ({})",
            share.id,
            status,
            share.tool,
            created,
            share.url()
        );
    }

    Ok(())
}

/// Delete a specific share
fn unshare(id: &str) -> Result<()> {
    let share = shares::get_share(id)?;

    match share {
        Some(share) => {
            // Delete from server
            println!("Deleting share {id} from server...");
            match delete_share(&share) {
                Ok(()) => println!("Deleted from server."),
                Err(e) => println!("Server delete failed (may already be gone): {e}"),
            }

            // Remove from local storage
            shares::remove_share(id)?;
            println!("Removed from local storage.");
            Ok(())
        }
        None => {
            bail!("Share not found: {id}");
        }
    }
}

/// Interactive TUI for managing shares
fn interactive() -> Result<()> {
    let theme = ColorfulTheme::default();

    loop {
        let shares = shares::load_shares()?;

        if shares.is_empty() {
            println!("No shares found.");
            return Ok(());
        }

        // Build selection items
        let format = format_description::parse("[year]-[month]-[day] [hour]:[minute]")?;
        let items: Vec<String> = shares
            .iter()
            .map(|s| {
                let status = if s.is_expired() { "EXPIRED" } else { "active" };
                let created = s.created_at.format(&format).unwrap_or_default();
                format!("[{}] {} {} - {}", status, s.id, s.tool, created)
            })
            .collect();

        // Add exit option
        let mut menu_items = items.clone();
        menu_items.push("Exit".to_string());

        let selection = Select::with_theme(&theme)
            .with_prompt("Select a share to manage")
            .items(&menu_items)
            .default(0)
            .interact()?;

        // Exit selected
        if selection == shares.len() {
            break;
        }

        let share = &shares[selection];

        // Show share details and actions
        println!("\n--- Share Details ---");
        println!("ID:         {}", share.id);
        println!("URL:        {}", share.url());
        println!("Tool:       {}", share.tool);
        println!(
            "Created:    {}",
            share.created_at.format(&format).unwrap_or_default()
        );
        println!(
            "Expires:    {}",
            share.expires_at.format(&format).unwrap_or_default()
        );
        println!(
            "Status:     {}",
            if share.is_expired() {
                "EXPIRED"
            } else {
                "active"
            }
        );
        println!("Transcript: {}", share.transcript_path);
        println!();

        let actions = vec!["Copy URL", "Open in browser", "Unshare (delete)", "Back"];
        let action = Select::with_theme(&theme)
            .with_prompt("Action")
            .items(&actions)
            .default(0)
            .interact()?;

        match action {
            0 => {
                // Copy URL - just print it (user can pipe to pbcopy)
                println!("\n{}\n", share.url());
            }
            1 => {
                // Open in browser
                #[cfg(target_os = "macos")]
                {
                    let _ = std::process::Command::new("open").arg(share.url()).spawn();
                }
                #[cfg(target_os = "linux")]
                {
                    let _ = std::process::Command::new("xdg-open")
                        .arg(share.url())
                        .spawn();
                }
                println!("Opened in browser.");
            }
            2 => {
                // Unshare
                let confirm = dialoguer::Confirm::with_theme(&theme)
                    .with_prompt("Are you sure you want to delete this share?")
                    .default(false)
                    .interact()?;

                if confirm {
                    let id = share.id.clone();
                    unshare(&id)?;
                }
            }
            _ => {
                // Back
            }
        }
    }

    Ok(())
}

fn delete_share(share: &Share) -> Result<()> {
    if share.storage_type == StorageType::Gist {
        delete_from_gist(share)
    } else {
        delete_from_server(share)
    }
}

fn delete_from_gist(share: &Share) -> Result<()> {
    let output = std::process::Command::new("gh")
        .args(["api", "-X", "DELETE", &format!("gists/{}", share.id)])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("GitHub delete failed: {}", stderr.trim());
    }

    Ok(())
}

/// Delete blob from server using the delete token
fn delete_from_server(share: &Share) -> Result<()> {
    let endpoint = format!("{}/blob/{}", share.upload_url, share.id);

    let response = ureq::delete(&endpoint)
        .set("X-Delete-Token", &share.delete_token)
        .call()?;

    if response.status() >= 400 {
        let status = response.status();
        bail!("Delete failed with status {status}");
    }

    Ok(())
}
