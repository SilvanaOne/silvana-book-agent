//! Interactive transaction confirmation for CLI --confirm mode
//!
//! Serializes stdin prompts across concurrent settlement tasks using a Mutex.
//! Prints prompts to stderr so they don't mix with structured logs.

use anyhow::{anyhow, Result};
use std::io::{BufRead, IsTerminal, Write};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Lock for serializing stdin prompts across concurrent tasks
pub type ConfirmLock = Arc<Mutex<()>>;

/// Create a new ConfirmLock
pub fn new_confirm_lock() -> ConfirmLock {
    Arc::new(Mutex::new(()))
}

/// Prompt the user to confirm a transaction before signing.
///
/// Prints to stderr (avoids mixing with logs/stdout).
/// Reads from stdin. If stdin is not a terminal (piped/redirected), auto-declines.
/// Returns `Ok(())` if confirmed, `Err` if declined or non-interactive.
///
/// The `lock` serializes prompts so concurrent settlement tasks don't interleave.
pub async fn confirm_transaction(
    lock: &ConfirmLock,
    action: &str,
    details: &str,
) -> Result<()> {
    let _guard = lock.lock().await;

    if !std::io::stdin().is_terminal() {
        return Err(anyhow!(
            "Non-interactive stdin — auto-declining confirmation for: {} ({})",
            action,
            details
        ));
    }

    let action = action.to_string();
    let details = details.to_string();

    tokio::task::spawn_blocking(move || {
        let mut stderr = std::io::stderr();
        write!(
            stderr,
            "\n[CONFIRM] {} — {}\n  Sign and submit? [y/N]: ",
            action, details
        )
        .ok();
        stderr.flush().ok();

        let mut input = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut input)
            .map_err(|e| anyhow!("Failed to read stdin: {}", e))?;

        let trimmed = input.trim().to_lowercase();
        if trimmed == "y" || trimmed == "yes" {
            Ok(())
        } else {
            Err(anyhow!("User declined: {} ({})", action, details))
        }
    })
    .await
    .map_err(|e| anyhow!("spawn_blocking failed: {}", e))?
}
