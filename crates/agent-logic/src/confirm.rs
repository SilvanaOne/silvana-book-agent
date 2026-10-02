//! Interactive transaction confirmation for CLI --confirm mode
//!
//! Serializes stdin prompts across concurrent settlement tasks using a Mutex.
//! Prints prompts to stderr so they don't mix with structured logs.

use anyhow::{anyhow, Result};
use std::future::Future;
use std::io::{BufRead, IsTerminal, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, OnceCell};

/// Lock for serializing stdin prompts across concurrent tasks
pub type ConfirmLock = Arc<Mutex<()>>;

/// Longest wait for a prompt's turn, and then for its answer; the transaction is then declined.
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(120);

/// Lines typed this long after a prompt timed out still belong to it.
const LATE_ANSWER_GRACE: Duration = Duration::from_secs(2);

/// Empty reads closer together than this are one repeat, not separate answers.
const REPEAT_WINDOW: Duration = Duration::from_millis(100);

/// Pause after a repeated empty read, so a closed terminal cannot spin.
const REPEAT_BACKOFF: Duration = Duration::from_millis(250);

/// Lines typed on stdin, read by one thread for the whole process.
type Answers = Mutex<mpsc::UnboundedReceiver<String>>;

static STDIN_ANSWERS: OnceCell<Answers> = OnceCell::const_new();

/// A prompt nobody answered: no turn, no answer in time, or no terminal to answer from.
#[derive(Debug)]
pub struct Unanswered(pub String);

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unanswered {}

/// Create a new ConfirmLock
pub fn new_confirm_lock() -> ConfirmLock {
    Arc::new(Mutex::new(()))
}

/// Prompt the user to confirm a transaction before signing.
///
/// Prints to stderr (avoids mixing with logs/stdout).
/// Reads from stdin. If stdin is not a terminal (piped/redirected), auto-declines.
/// Waiting over `CONFIRM_TIMEOUT` for the turn or for the answer also declines.
/// Returns `Ok(())` if confirmed, `Err` if declined or non-interactive.
///
/// The `lock` serializes prompts so concurrent settlement tasks don't interleave.
pub async fn confirm_transaction(
    lock: &ConfirmLock,
    action: &str,
    details: &str,
) -> Result<()> {
    let answers = || async {
        if !std::io::stdin().is_terminal() {
            return Err(Unanswered(format!(
                "Non-interactive stdin — auto-declining confirmation for: {} ({})",
                action, details
            ))
            .into());
        }
        STDIN_ANSWERS.get_or_try_init(|| async { start_stdin_reader() }).await
    };
    confirm_with(lock, answers, action, details, CONFIRM_TIMEOUT, LATE_ANSWER_GRACE).await
}

/// The stdin lines, from a reader thread that runs as long as they are read.
fn start_stdin_reader() -> Result<Answers> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("confirm-stdin".to_string())
        .spawn(move || pump_lines(std::io::stdin().lock(), &tx, REPEAT_BACKOFF))
        .map_err(|e| anyhow!("Failed to start the stdin reader: {}", e))?;
    Ok(Mutex::new(rx))
}

/// Send each line of `input` until nobody receives them; an end of input or an
/// unreadable line is sent as an empty line, which declines.
fn pump_lines<R: BufRead>(mut input: R, tx: &mpsc::UnboundedSender<String>, backoff: Duration) {
    let mut last_empty: Option<Instant> = None;
    while !tx.is_closed() {
        let mut line = String::new();
        if !matches!(input.read_line(&mut line), Ok(n) if n > 0) {
            line.clear();
            if last_empty.is_some_and(|at| at.elapsed() < REPEAT_WINDOW) {
                // A repeat is dropped, so a closed terminal cannot spin or flood
                std::thread::sleep(backoff);
                last_empty = Some(Instant::now());
                continue;
            }
            last_empty = Some(Instant::now());
        }
        if tx.send(line).is_err() {
            break;
        }
    }
}

/// [`confirm_transaction`] with the answer lines from `answers` and both waits bounded by `limit`;
/// after a timeout, lines typed within `grace` are dropped.
async fn confirm_with<'a, F, Fut>(
    lock: &ConfirmLock,
    answers: F,
    action: &str,
    details: &str,
    limit: Duration,
    grace: Duration,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<&'a Answers>>,
{
    let turn_by = crate::clock::deadline_after(limit);
    let no_turn = || -> anyhow::Error {
        Unanswered(format!(
            "no confirmation slot within {:?} — declined: {} ({})",
            limit, action, details
        ))
        .into()
    };
    let Ok(_guard) = tokio::time::timeout_at(turn_by, lock.lock()).await else {
        return Err(no_turn());
    };
    let answers = answers().await?;
    let Ok(mut lines) = tokio::time::timeout_at(turn_by, answers.lock()).await else {
        return Err(no_turn());
    };
    // A late answer to an earlier prompt must not approve this one
    while lines.try_recv().is_ok() {}

    {
        let mut stderr = std::io::stderr().lock();
        write!(stderr, "\n[CONFIRM] {} — {}\n  Sign and submit? [y/N]: ", action, details).ok();
        stderr.flush().ok();
    }

    match tokio::time::timeout(limit, lines.recv()).await {
        Ok(Some(input)) => {
            let trimmed = input.trim().to_lowercase();
            if trimmed == "y" || trimmed == "yes" {
                Ok(())
            } else {
                Err(anyhow!("User declined: {} ({})", action, details))
            }
        }
        Ok(None) => Err(Unanswered(format!("stdin closed — declined: {} ({})", action, details)).into()),
        Err(_) => {
            crate::errln!("[CONFIRM] no answer within {:?} — declined", limit);
            // Hold the turn briefly so a late answer to this prompt is not taken by the next
            let quiet_until = crate::clock::deadline_after(grace);
            while let Ok(Some(_)) = tokio::time::timeout_at(quiet_until, lines.recv()).await {}
            Err(Unanswered(format!(
                "no answer within {:?} — declined: {} ({})",
                limit, action, details
            ))
            .into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    /// A short bound for tests; production passes [`CONFIRM_TIMEOUT`].
    const LIMIT: Duration = Duration::from_millis(300);
    /// The late-answer grace for tests; production passes [`LATE_ANSWER_GRACE`].
    const GRACE: Duration = Duration::from_millis(500);

    /// The call's error, failing the test if it is still waiting long after the bound.
    async fn declined(call: impl Future<Output = Result<()>>) -> String {
        let bounded = tokio::time::timeout(Duration::from_secs(20), call).await;
        bounded.expect("the prompt ends within its bound").unwrap_err().to_string()
    }

    fn assert_took_the_bound(started: Instant) {
        let took = started.elapsed();
        assert!(took >= LIMIT && took < Duration::from_secs(10), "{took:?}");
    }

    #[test]
    fn prompts_are_bounded_at_two_minutes() {
        assert_eq!(CONFIRM_TIMEOUT, Duration::from_secs(120));
        assert_eq!(LATE_ANSWER_GRACE, Duration::from_secs(2));
    }

    // An unanswered prompt used to hold its lock and its caller forever
    #[tokio::test]
    async fn an_unanswered_prompt_is_declined_within_the_bound() {
        let (_tx, rx) = mpsc::unbounded_channel::<String>();
        let answers = Mutex::new(rx);
        let lock = new_confirm_lock();
        let started = Instant::now();
        let err = declined(confirm_with(&lock, || async { Ok(&answers) }, "Allocate", "p1", LIMIT, GRACE)).await;
        assert!(err.contains("no answer within 300ms") && err.contains("Allocate (p1)"), "{err}");
        assert_took_the_bound(started);
        assert!(lock.try_lock().is_ok(), "the turn is given up");
    }

    // A prompt waiting behind an unanswered one used to wait forever for its turn
    #[tokio::test]
    async fn a_prompt_waiting_for_its_turn_is_declined_within_the_bound() {
        let (_tx, rx) = mpsc::unbounded_channel::<String>();
        let answers = Mutex::new(rx);
        let lock = new_confirm_lock();
        let first = lock.clone().lock_owned().await;
        let started = Instant::now();
        let call = confirm_with(&lock, || async { Ok(&answers) }, "Propose DVP", "p2", LIMIT, GRACE);
        let err = declined(call).await;
        assert!(err.contains("no confirmation slot within 300ms") && err.contains("Propose DVP (p2)"), "{err}");
        assert_took_the_bound(started);

        // The same holds for the stdin lines held by a prompt under another lock
        let held_lines = answers.lock().await;
        let started = Instant::now();
        let other = new_confirm_lock();
        let err = declined(confirm_with(&other, || async { Ok(&answers) }, "Accept DVP", "p3", LIMIT, GRACE)).await;
        assert!(err.contains("no confirmation slot"), "{err}");
        assert_took_the_bound(started);
        drop((first, held_lines));
    }

    // A late answer typed for an earlier prompt used to approve the next one
    #[tokio::test]
    async fn an_answer_typed_before_the_prompt_is_not_approval() {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        tx.send("y\n".to_string()).unwrap();
        let answers = Mutex::new(rx);
        let lock = new_confirm_lock();
        let err = declined(confirm_with(&lock, || async { Ok(&answers) }, "PayFee", "p4", LIMIT, GRACE)).await;
        assert!(err.contains("no answer within"), "{err}");
    }

    /// A prompt that times out at [`LIMIT`], a second one queued 50ms behind it, and
    /// "y" typed at each of `late` after the first one's deadline; the second one's outcome.
    async fn next_prompt_after_late_answers(late: &[Duration]) -> Result<()> {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let answers = Mutex::new(rx);
        let lock = new_confirm_lock();
        let started = tokio::time::Instant::now();
        let first = confirm_with(&lock, || async { Ok(&answers) }, "Allocate", "p1", LIMIT, GRACE);
        let second = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let limit = Duration::from_secs(1);
            confirm_with(&lock, || async { Ok(&answers) }, "Propose DVP", "p2", limit, GRACE).await
        };
        let typed = async {
            for after in late {
                tokio::time::sleep_until(started + LIMIT + *after).await;
                tx.send("y\n".to_string()).unwrap();
            }
        };
        // Lines due by a wakeup are sent before either prompt sees it
        let ((), first, second) = tokio::join!(typed, first, second);
        assert!(first.is_err(), "the first prompt timed out");
        second
    }

    // An answer typed just after a prompt's deadline used to approve the prompt queued behind it
    #[tokio::test]
    async fn a_late_answer_never_approves_the_next_prompt() {
        let (first, second) = (Duration::from_millis(60), Duration::from_millis(160));
        let (once, twice) = (vec![first], vec![first, second]);
        let (once, twice) = tokio::join!(next_prompt_after_late_answers(&once), next_prompt_after_late_answers(&twice));
        assert!(once.is_err(), "a late answer is dropped");
        assert!(twice.is_err(), "every line within the grace is dropped");
    }

    #[tokio::test]
    async fn an_answer_after_the_prompt_decides_it() {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let answers = Mutex::new(rx);
        let lock = new_confirm_lock();
        let generous = Duration::from_secs(20);
        let call = confirm_with(&lock, || async { Ok(&answers) }, "Allocate", "p5", generous, GRACE);
        let answer = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(" Yes \n".to_string()).unwrap();
        };
        let (approved, ()) = tokio::join!(call, answer);
        approved.unwrap();

        let call = confirm_with(&lock, || async { Ok(&answers) }, "Allocate", "p6", generous, GRACE);
        let answer = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send("n\n".to_string()).unwrap();
        };
        let (refused, ()) = tokio::join!(call, answer);
        assert!(refused.unwrap_err().to_string().contains("User declined: Allocate (p6)"));

        drop(tx);
        let err = declined(confirm_with(&lock, || async { Ok(&answers) }, "Allocate", "p7", generous, GRACE)).await;
        assert!(err.contains("stdin closed"), "{err}");
    }

    #[tokio::test]
    async fn a_failed_answer_source_declines() {
        let lock = new_confirm_lock();
        let call = confirm_with(&lock, || async { Err(anyhow!("no terminal")) }, "Allocate", "p8", LIMIT, GRACE);
        assert_eq!(call.await.unwrap_err().to_string(), "no terminal");
        assert!(lock.try_lock().is_ok());
    }

    // A prompt nobody answered is told apart from a refusal, so a caller can stop instead of retrying
    #[tokio::test]
    async fn only_a_prompt_nobody_answered_is_unanswered() {
        let unanswered = |e: &anyhow::Error| e.downcast_ref::<Unanswered>().is_some();
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let answers = Mutex::new(rx);
        let lock = new_confirm_lock();
        let err = confirm_with(&lock, || async { Ok(&answers) }, "Allocate", "p1", LIMIT, GRACE).await.unwrap_err();
        assert!(unanswered(&err) && err.to_string().contains("no answer within"), "{err}");

        let held = lock.clone().lock_owned().await;
        let err = confirm_with(&lock, || async { Ok(&answers) }, "Allocate", "p2", LIMIT, GRACE).await.unwrap_err();
        assert!(unanswered(&err) && err.to_string().contains("no confirmation slot"), "{err}");
        drop(held);

        let call = confirm_with(&lock, || async { Ok(&answers) }, "Allocate", "p3", Duration::from_secs(20), GRACE);
        let answer = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send("n\n".to_string()).unwrap();
        };
        let (refused, ()) = tokio::join!(call, answer);
        let err = refused.unwrap_err();
        assert!(!unanswered(&err) && err.to_string().contains("User declined"), "{err}");

        drop(tx);
        let err = confirm_with(&lock, || async { Ok(&answers) }, "Allocate", "p4", LIMIT, GRACE).await.unwrap_err();
        assert!(unanswered(&err) && err.to_string().contains("stdin closed"), "{err}");
    }

    /// A terminal read from a script, one chunk per read; an empty chunk is a Ctrl-D.
    struct Tty(std::sync::mpsc::Receiver<Vec<u8>>);

    impl std::io::Read for Tty {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let chunk = self.0.recv().map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
            let n = chunk.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk[..n]);
            Ok(n)
        }
    }

    /// Answer lines pumped from a scripted terminal, and the sender of its script.
    fn scripted(backoff: Duration) -> (std::sync::mpsc::Sender<Vec<u8>>, Answers) {
        let (script, chunks) = std::sync::mpsc::channel();
        let (tx, rx) = mpsc::unbounded_channel();
        let input = std::io::BufReader::new(Tty(chunks));
        std::thread::Builder::new().spawn(move || pump_lines(input, &tx, backoff)).unwrap();
        (script, Mutex::new(rx))
    }

    /// A prompt on `answers`, with `typed` sent to the terminal once it is shown.
    async fn prompt_answered(answers: &Answers, script: &std::sync::mpsc::Sender<Vec<u8>>, typed: &[u8]) -> Result<()> {
        let lock = new_confirm_lock();
        let call = confirm_with(&lock, || async { Ok(answers) }, "Allocate", "p1", Duration::from_secs(20), GRACE);
        let answer = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            script.send(typed.to_vec()).unwrap();
        };
        let (answered, ()) = tokio::join!(call, answer);
        answered
    }

    // A Ctrl-D or an unreadable line used to end the stdin reader, declining every later prompt at once
    #[tokio::test]
    async fn a_ctrl_d_or_an_unreadable_line_declines_only_its_own_prompt() {
        for refusal in [&b""[..], &b"\xff\xfe\n"[..]] {
            let (script, answers) = scripted(REPEAT_BACKOFF);
            let refused = prompt_answered(&answers, &script, refusal).await.unwrap_err();
            assert!(refused.to_string().contains("User declined: Allocate (p1)"), "{refusal:?}: {refused}");
            prompt_answered(&answers, &script, b"y\n").await.unwrap();
        }
    }

    // Two Ctrl-Ds in a row count as one, and the terminal still answers later prompts
    #[tokio::test]
    async fn back_to_back_ctrl_ds_leave_the_terminal_answering() {
        let (script, answers) = scripted(Duration::from_millis(50));
        script.send(Vec::new()).unwrap();
        script.send(Vec::new()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        prompt_answered(&answers, &script, b"y\n").await.unwrap();
    }

    /// A terminal whose every read is an end of input; counts the reads.
    struct Closed(Arc<AtomicUsize>);

    impl std::io::Read for Closed {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            self.0.fetch_add(1, SeqCst);
            Ok(0)
        }
    }

    // A closed terminal sends one empty line, then backs off instead of spinning
    #[test]
    fn a_closed_terminal_neither_spins_nor_floods() {
        let reads = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let input = std::io::BufReader::new(Closed(Arc::clone(&reads)));
        let backoff = Duration::from_millis(50);
        let pump = std::thread::Builder::new().spawn(move || pump_lines(input, &tx, backoff)).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let mut sent = Vec::new();
        while let Ok(line) = rx.try_recv() {
            sent.push(line);
        }
        assert_eq!(sent, [""]);
        let n = reads.load(SeqCst);
        assert!(n <= 15, "{n} reads in 500ms");

        drop(rx);
        let started = Instant::now();
        while !pump.is_finished() {
            assert!(started.elapsed() < Duration::from_secs(5), "the reader ends once nobody receives");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
