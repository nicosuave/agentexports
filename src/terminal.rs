//! Terminal identity: compute stable terminal key for session tracking.

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString};

/// Terminal identity components
#[derive(Debug, Clone)]
pub struct TerminalIdentity {
    pub tty: String,
    pub tmux_pane: Option<String>,
    pub iterm_session_id: Option<String>,
}

/// Compute a stable hash key from terminal identity components
pub fn compute_term_key(
    tty: &str,
    tmux_pane: Option<&str>,
    iterm_session_id: Option<&str>,
) -> String {
    let tmux = tmux_pane.unwrap_or("");
    let iterm = iterm_session_id.unwrap_or("");
    let input = format!("{tty}|{tmux}|{iterm}");
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

/// Get the current tty path
fn current_tty() -> Result<String> {
    unsafe {
        let ptr = libc::ttyname(libc::STDIN_FILENO);
        if !ptr.is_null() {
            let c_str = CStr::from_ptr(ptr);
            return Ok(c_str.to_str()?.to_string());
        }

        let dev_tty = CString::new("/dev/tty")?;
        let fd = libc::open(dev_tty.as_ptr(), libc::O_RDONLY);
        if fd < 0 {
            bail!("stdin is not a tty and /dev/tty unavailable; pass --term-key explicitly");
        }
        let ptr = libc::ttyname(fd);
        libc::close(fd);
        if ptr.is_null() {
            bail!("failed to resolve tty; pass --term-key explicitly");
        }
        let c_str = CStr::from_ptr(ptr);
        Ok(c_str.to_str()?.to_string())
    }
}

/// Get the current terminal identity
pub fn current_terminal_identity() -> Result<TerminalIdentity> {
    let tty = current_tty()?;
    let tmux_pane = std::env::var("TMUX_PANE").ok();
    let iterm_session_id = std::env::var("ITERM_SESSION_ID").ok();
    Ok(TerminalIdentity {
        tty,
        tmux_pane,
        iterm_session_id,
    })
}

/// Get the current terminal key (hash of terminal identity)
pub fn current_term_key() -> Result<String> {
    let identity = current_terminal_identity()?;
    Ok(compute_term_key(
        &identity.tty,
        identity.tmux_pane.as_deref(),
        identity.iterm_session_id.as_deref(),
    ))
}

/// Shell-quote a string for safe use in shell scripts
pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn term_key_hash_is_stable() {
        let key = compute_term_key("/dev/ttys007", Some("%1"), Some("ABC"));
        assert_eq!(
            key,
            "dab577fe0a6ec2761d461d687ee15471967cefa6d697e24f40f53db872caf1d7"
        );
    }

    // ===== shell_quote tests =====

    #[test]
    fn test_shell_quote_empty() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn test_shell_quote_simple() {
        assert_eq!(shell_quote("hello"), "'hello'");
    }

    #[test]
    fn test_shell_quote_with_spaces() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
    }

    #[test]
    fn test_shell_quote_with_single_quote() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_shell_quote_multiple_single_quotes() {
        assert_eq!(shell_quote("it's a 'test'"), "'it'\\''s a '\\''test'\\'''");
    }

    #[test]
    fn test_shell_quote_special_chars() {
        // Special chars other than single quote should be preserved inside quotes
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_quote("foo; bar"), "'foo; bar'");
        assert_eq!(shell_quote("foo && bar"), "'foo && bar'");
    }
}
