//! Typed confirmations for control changes, read from the terminal within a bound.

use std::collections::VecDeque;
use std::io::{BufRead, IsTerminal, Write};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

/// Longest wait for a typed answer.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);

/// Where typed answers come from.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait Prompter: Send + Sync {
    /// Whether a person can answer.
    fn interactive(&self) -> bool;
    /// One line without its newline; `None` when nothing arrived within `timeout`.
    async fn line(&self, timeout: Duration) -> Result<Option<String>>;
}

/// The process's stdin.
pub struct Terminal;

#[async_trait::async_trait]
impl Prompter for Terminal {
    fn interactive(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    async fn line(&self, timeout: Duration) -> Result<Option<String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        // A plain thread: a read left blocked must not hold the runtime's shutdown.
        std::thread::Builder::new()
            .name("control-prompt".into())
            .spawn(move || {
                let mut line = String::new();
                let read = std::io::stdin().lock().read_line(&mut line).map(|_| line);
                let _ = tx.send(read);
            })
            .map_err(|e| anyhow!("could not read the terminal: {e}"))?;
        match tokio::time::timeout(timeout, rx).await {
            Err(_) => Ok(None),
            Ok(Err(_)) => bail!("the terminal read stopped"),
            Ok(Ok(Err(e))) => bail!("could not read the terminal: {e}"),
            Ok(Ok(Ok(line))) if line.is_empty() => Ok(None),
            Ok(Ok(Ok(line))) => Ok(Some(line.trim_end_matches(['\r', '\n']).to_string())),
        }
    }
}

/// Answers given in advance, for tests and scripted runs.
pub struct Scripted {
    answers: Mutex<VecDeque<String>>,
    interactive: bool,
}

impl Scripted {
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(answers: I) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().map(Into::into).collect()),
            interactive: true,
        }
    }

    /// No terminal at all.
    pub fn none() -> Self {
        Self { answers: Mutex::new(VecDeque::new()), interactive: false }
    }
}

#[async_trait::async_trait]
impl Prompter for Scripted {
    fn interactive(&self) -> bool {
        self.interactive
    }

    async fn line(&self, _: Duration) -> Result<Option<String>> {
        Ok(agent_logic::sync::lock(&self.answers).pop_front())
    }
}

/// How `--yes` applies to one confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Yes {
    /// `--yes` was given and is accepted here.
    Given,
    /// Not given: the phrase must be typed.
    NotGiven,
    /// Never accepted, as with `--force`.
    Refused,
}

/// Requires `expected` typed exactly; `--yes` skips it only where accepted.
pub async fn confirm_typed(
    prompter: &dyn Prompter,
    out: &mut dyn Write,
    expected: &str,
    yes: Yes,
) -> Result<()> {
    if yes == Yes::Given {
        return Ok(());
    }
    if !prompter.interactive() {
        bail!(
            "no terminal to confirm on: this change needs the typed confirmation '{expected}'{}",
            if yes == Yes::NotGiven { " (or --yes)" } else { "" }
        );
    }
    let _ = write!(out, "Type '{expected}' to continue: ");
    let _ = out.flush();
    match prompter.line(PROMPT_TIMEOUT).await? {
        Some(answer) if answer.trim() == expected => Ok(()),
        Some(_) => bail!("not confirmed: the text typed was not '{expected}'; nothing was signed"),
        None => bail!("not confirmed within {PROMPT_TIMEOUT:?}; nothing was signed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_the_exact_phrase_confirms() {
        let mut out = Vec::new();
        confirm_typed(&Scripted::new(["link"]), &mut out, "link", Yes::NotGiven).await.unwrap();
        assert!(String::from_utf8_lossy(&out).contains("Type 'link'"));
        for typed in ["Link", "lin", "yes"] {
            let err = confirm_typed(&Scripted::new([typed]), &mut out, "link", Yes::NotGiven).await.unwrap_err();
            assert!(err.to_string().contains("not confirmed"), "{err}");
        }
        assert!(confirm_typed(&Scripted::new(Vec::<String>::new()), &mut out, "link", Yes::NotGiven).await.is_err());
    }

    #[tokio::test]
    async fn yes_skips_only_where_accepted_and_no_terminal_refuses() {
        let mut out = Vec::new();
        confirm_typed(&Scripted::none(), &mut out, "add", Yes::Given).await.unwrap();
        let err = confirm_typed(&Scripted::none(), &mut out, "add", Yes::NotGiven).await.unwrap_err();
        assert!(err.to_string().contains("(or --yes)"), "{err}");
        let err = confirm_typed(&Scripted::none(), &mut out, "freeze x", Yes::Refused).await.unwrap_err();
        assert!(!err.to_string().contains("--yes"), "{err}");
    }
}
