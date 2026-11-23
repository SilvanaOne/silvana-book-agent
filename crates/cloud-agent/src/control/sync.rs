//! Brings the local and remote control logs together before any change, and the
//! `sync`, `restore` and `discard-pending` commands.

use std::collections::BTreeSet;
use std::io::Write;

use agent_control::{
    ControlState, EntryBody, Hash32, HeadRef, LogComparison, MAX_APPEND_DOCUMENTS, MAX_APPEND_ENTRIES,
    PendingEntry, SignedEntry, compare_logs, replay, verify_ed25519_strict,
};
use agent_logic::control_store::{
    ControlLock, ControlMarker, EVIDENCE_DIR, HEAD_FILE, LOG_FILE, PENDING_FILE, PendingRecord,
    PendingStatus, StoredLog,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use super::prompt::{Prompter, Yes, confirm_typed};
use super::rpc::{ControlRpc, RemoteLog, RpcError, document_hash};
use super::{ControlContext, RemoteView, Standing, standing, verify_remote};

/// The rpc's skew refusal: a re-uploaded entry is older than it accepts.
const REFUSED_SKEW: u16 = 4090;
/// Refusals the rpc gives a documents-only upload whose documents it still stores
/// (a failing log reads as a gap, missing documents as malformed).
const DOCUMENTS_KEPT: [u16; 2] = [4084, 4091];

/// Reports a log disagreement to the operator and to the error feed.
fn report_critical(ctx: &ControlContext, error_type: &str, message: &str) {
    tracing::error!("agent control: {message}");
    agent_logic::error_reporter::ErrorEventBuilder::new(error_type, message)
        .severity("critical")
        .party(ctx.party.as_str())
        .module("control")
        .send();
}

fn head_text(head: Option<HeadRef>) -> String {
    match head {
        Some(head) => format!("seq {} ({})", head.seq, head.hash),
        None => "empty".to_string(),
    }
}

fn head_of(entry: &SignedEntry) -> HeadRef {
    HeadRef { seq: entry.seq(), hash: *entry.hash() }
}

/// The local and remote logs agree, under the lock: changes may be signed.
pub struct Prepared {
    lock: ControlLock,
    pub state: ControlState,
    pub remote_head: Option<HeadRef>,
    pub uploaded: usize,
}

impl Prepared {
    pub fn head(&self) -> Option<HeadRef> {
        self.state.head()
    }

    pub fn lock(&self) -> &ControlLock {
        &self.lock
    }

    pub fn into_lock(self) -> ControlLock {
        self.lock
    }

    /// Releases the lock, keeping what was read.
    pub fn release(self) -> (ControlState, Option<HeadRef>) {
        let Self { lock, state, remote_head, .. } = self;
        drop(lock);
        (state, remote_head)
    }
}

/// The replayed state of `stored`, or the empty state.
pub(crate) fn state_of(ctx: &ControlContext, stored: Option<&StoredLog>) -> Result<ControlState> {
    match stored {
        None => Ok(ControlState::empty(ctx.network, ctx.party.clone(), ctx.signer.public_key())),
        Some(stored) => replay(&stored.log, &stored.documents).map_err(|e| {
            anyhow!(
                "the local control log does not replay at seq {}: {} (run `cloud-agent control restore`)",
                e.seq,
                e.code_name()
            )
        }),
    }
}

/// The policy document a local entry accepts, if it accepts one.
fn document_of(entry: &SignedEntry, stored: &StoredLog) -> Result<Option<(Hash32, Vec<u8>)>> {
    let EntryBody::AcceptPolicy(body) = entry.entry().body() else {
        return Ok(None);
    };
    let bytes = stored
        .documents
        .get(&body.policy_hash)
        .ok_or_else(|| anyhow!("policy document {} of seq {} is missing locally", body.policy_hash, entry.seq()))?;
    Ok(Some((body.policy_hash, bytes.to_vec())))
}

/// Runs of consecutive seqs, as (first, last).
fn runs_of(seqs: &[u64]) -> Vec<(u64, u64)> {
    let mut runs: Vec<(u64, u64)> = Vec::new();
    for &seq in seqs {
        match runs.last_mut() {
            Some((_, last)) if last.checked_add(1) == Some(seq) => *last = seq,
            _ => runs.push((seq, seq)),
        }
    }
    runs
}

/// Why an upload of local entries from `from` failed; a skew refusal of entries the rpc
/// once held means its log and heads row were restored together.
fn upload_failed(ctx: &ControlContext, stored: &StoredLog, error: &RpcError, from: u64) -> anyhow::Error {
    if error.definite_code() == Some(REFUSED_SKEW) {
        let head = stored.log.head();
        return anyhow!(
            "uploading the control log failed: the rpc refused the entries from seq {from} as too old \
             (request refused (code {REFUSED_SKEW})). Its control log and heads row look restored from a \
             backup: ask the rpc operator to re-seed agent_control_heads for {} (head_seq/head_hash from \
             claimed_seq/claimed_hash) after confirming the head {} with you, then run \
             `cloud-agent control sync` again",
            ctx.party,
            head_text(Some(head))
        );
    }
    anyhow!("uploading the control log failed: {error}")
}

/// The head seq the rpc reaches after an upload ending at `end`: it re-verifies its stored
/// rows above a partial repair, up to the next bad seq, its tip and the local head.
fn reverified_tip(bad: &[u64], end: u64, remote_tip: u64, local_head: u64) -> u64 {
    let reach = bad.iter().find(|seq| **seq > end).map_or(u64::MAX, |seq| seq.saturating_sub(1));
    reach.min(remote_tip).min(local_head).max(end)
}

/// One upload of the local log: entries in runs and documents in batches, every request
/// within MAX_APPEND_ENTRIES and MAX_APPEND_DOCUMENTS, documents deduplicated by hash.
struct Upload<'a> {
    ctx: &'a ControlContext,
    rpc: &'a ControlRpc,
    lock: &'a ControlLock,
    stored: &'a StoredLog,
    /// Local seqs the rpc lacks or holds broken, ascending.
    bad: &'a [u64],
    /// The highest seq the rpc stores or reports.
    remote_tip: u64,
    /// Documents the rpc holds or was sent.
    held: BTreeSet<Hash32>,
    /// Entries sent.
    sent: usize,
}

impl Upload<'_> {
    /// Documents of the entries through `through` the rpc lacks, sent with that stored entry;
    /// the rpc keeps them on every outcome, so only a last batch on a `whole` log must pass.
    async fn send_documents(&mut self, through: u64, whole: bool) -> Result<()> {
        let mut missing: Vec<(Hash32, Vec<u8>)> = Vec::new();
        for entry in self.stored.log.entries().iter().take_while(|e| e.seq() <= through) {
            if let Some((hash, bytes)) = document_of(entry, self.stored)?
                && !self.held.contains(&hash)
                && !missing.iter().any(|(seen, _)| *seen == hash)
            {
                missing.push((hash, bytes));
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        let anchor = self
            .stored
            .log
            .entry(through)
            .cloned()
            .ok_or_else(|| anyhow!("internal: no local entry at seq {through}"))?;
        let expected = head_of(&anchor);
        let batches: Vec<&[(Hash32, Vec<u8>)]> = missing.chunks(MAX_APPEND_DOCUMENTS).collect();
        let count = batches.len();
        for (index, batch) in batches.into_iter().enumerate() {
            let last = index.saturating_add(1) == count;
            let documents: Vec<Vec<u8>> = batch.iter().map(|(_, bytes)| bytes.clone()).collect();
            let answer = self
                .rpc
                .append_retrying(&self.ctx.store, self.lock, std::slice::from_ref(&anchor), &documents)
                .await;
            match answer {
                Ok(head) if head == expected => {}
                Ok(head) => bail!(
                    "the rpc reports head {} after re-sending policy documents at {}",
                    head_text(Some(head)),
                    head_text(Some(expected))
                ),
                Err(e) if !(last && whole) && e.definite_code().is_some_and(|c| DOCUMENTS_KEPT.contains(&c)) => {
                    tracing::debug!("agent control: documents sent with seq {through} ({e}); they are kept");
                }
                Err(e) => bail!("re-sending policy documents failed: {e}"),
            }
            self.held.extend(batch.iter().map(|(hash, _)| *hash));
        }
        Ok(())
    }

    /// Local entries `from..=to`, in requests each claiming the head the rpc then reaches.
    async fn send_run(&mut self, from: u64, to: u64) -> Result<()> {
        let entries: Vec<&SignedEntry> =
            self.stored.log.entries().iter().filter(|e| (from..=to).contains(&e.seq())).collect();
        let mut rest: &[&SignedEntry] = &entries;
        while !rest.is_empty() {
            let mut chunk: Vec<SignedEntry> = Vec::new();
            let mut documents: Vec<(Hash32, Vec<u8>)> = Vec::new();
            for entry in rest.iter().take(MAX_APPEND_ENTRIES) {
                if let Some((hash, bytes)) = document_of(entry, self.stored)?
                    && !self.held.contains(&hash)
                    && !documents.iter().any(|(seen, _)| *seen == hash)
                {
                    if documents.len() >= MAX_APPEND_DOCUMENTS {
                        break;
                    }
                    documents.push((hash, bytes));
                }
                chunk.push((*entry).clone());
            }
            let (Some(first), Some(last)) = (chunk.first(), chunk.last()) else {
                bail!("internal: an empty upload chunk");
            };
            let (first_seq, last_seq) = (first.seq(), last.seq());
            let tip = reverified_tip(self.bad, last_seq, self.remote_tip, self.stored.log.head().seq);
            let expected = self
                .stored
                .log
                .entry(tip)
                .map(head_of)
                .ok_or_else(|| anyhow!("internal: no local entry at seq {tip}"))?;
            // The rows the rpc verifies again need their documents in the same request.
            for entry in self.stored.log.entries().iter().filter(|e| e.seq() > last_seq && e.seq() <= tip) {
                if let Some((hash, bytes)) = document_of(entry, self.stored)?
                    && !self.held.contains(&hash)
                    && !documents.iter().any(|(seen, _)| *seen == hash)
                    && documents.len() < MAX_APPEND_DOCUMENTS
                {
                    documents.push((hash, bytes));
                }
            }
            let bodies: Vec<Vec<u8>> = documents.iter().map(|(_, bytes)| bytes.clone()).collect();
            let head = self
                .rpc
                .append_retrying_claiming(&self.ctx.store, self.lock, &chunk, &bodies, &expected)
                .await
                .map_err(|e| upload_failed(self.ctx, self.stored, &e, first_seq))?;
            ensure!(
                head == expected,
                "the rpc reports head {} after an upload ending at seq {last_seq}, not {}",
                head_text(Some(head)),
                head_text(Some(expected))
            );
            self.held.extend(documents.iter().map(|(hash, _)| *hash));
            self.sent = self.sent.saturating_add(chunk.len());
            rest = rest.get(chunk.len()..).unwrap_or_default();
        }
        Ok(())
    }
}

/// The runs to upload: one request from the first bad seq through the rpc's tip when it
/// fits (any planner accepts it), else each run of bad seqs (a partial repair each).
fn upload_runs(stored: &StoredLog, held: &BTreeSet<Hash32>, bad: &[u64], remote_tip: u64) -> Result<Vec<(u64, u64)>> {
    let (Some(&first), Some(&last)) = (bad.first(), bad.last()) else {
        return Ok(Vec::new());
    };
    let end = last.max(remote_tip.min(stored.log.head().seq));
    let span: Vec<&SignedEntry> = stored.log.entries().iter().filter(|e| (first..=end).contains(&e.seq())).collect();
    let mut documents = BTreeSet::new();
    for entry in &span {
        if let Some((hash, _)) = document_of(entry, stored)?
            && !held.contains(&hash)
        {
            documents.insert(hash);
        }
    }
    if span.len() <= MAX_APPEND_ENTRIES && documents.len() <= MAX_APPEND_DOCUMENTS {
        return Ok(vec![(first, end)]);
    }
    Ok(runs_of(bad))
}

/// Uploads each run after the documents of the entries before it, then the documents
/// the rpc still lacks; returns the entries sent.
async fn upload_missing(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    lock: &ControlLock,
    stored: &StoredLog,
    remote: &RemoteLog,
    bad: &[u64],
) -> Result<usize> {
    let held: BTreeSet<Hash32> = remote.documents.iter().map(|d| document_hash(d)).collect();
    let remote_tip = remote.tip();
    let runs = upload_runs(stored, &held, bad, remote_tip)?;
    let mut upload = Upload { ctx, rpc, lock, stored, bad, remote_tip, held, sent: 0 };
    for (from, to) in runs {
        if let Some(before) = from.checked_sub(1).filter(|seq| *seq >= 1) {
            upload.send_documents(before, false).await?;
        }
        upload.send_run(from, to).await?;
    }
    upload.send_documents(stored.log.head().seq, true).await?;
    Ok(upload.sent)
}

/// Appends the pending entry the rpc holds and moves the marker; the guard reloads later.
fn accept_pending_locked<W: Write + ?Sized>(ctx: &ControlContext, lock: &ControlLock, pending: &PendingRecord, out: &mut W) -> Result<HeadRef> {
    let accepted = head_of(&pending.entry);
    let head = ctx.store.accept_pending(lock, &accepted)?;
    ctx.mark(head, out)?;
    let _ = writeln!(out, "The rpc holds the pending entry at seq {}: appended locally.", head.seq);
    Ok(head)
}

/// What a definite refusal of the pending entry means; anything else leaves it pending.
fn pending_refused(ctx: &ControlContext, lock: &ControlLock, error: &RpcError, seq: u64, now: i64) -> Result<anyhow::Error> {
    match error.definite_code() {
        Some(code) => {
            ctx.store.refuse_pending(lock, Some(code), now)?;
            Ok(anyhow!(
                "the rpc refused the signed entry at seq {seq}: request refused (code {code}). It is kept \
                 and seq {seq} is not signed again automatically; see `cloud-agent control discard-pending`"
            ))
        }
        None => Ok(anyhow!("{error}; the signed entry at seq {seq} stays pending: run `cloud-agent control sync`")),
    }
}

/// Uploads the pending entry; a definite refusal is recorded and never re-signed, and a
/// transient one is retried with backoff and leaves it pending.
async fn upload_pending<W: Write + ?Sized>(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    lock: &ControlLock,
    pending: &PendingRecord,
    out: &mut W,
) -> Result<HeadRef> {
    let documents: Vec<Vec<u8>> = pending.document.iter().cloned().collect();
    let expected = head_of(&pending.entry);
    match rpc.append_retrying(&ctx.store, lock, std::slice::from_ref(&pending.entry), &documents).await {
        Ok(head) if head == expected => accept_pending_locked(ctx, lock, pending, out),
        Ok(head) => bail!(
            "the rpc reports head {} for the pending entry at seq {}; it stays pending: run `cloud-agent control sync`",
            head_text(Some(head)),
            expected.seq
        ),
        Err(e) => Err(pending_refused(ctx, lock, &e, expected.seq, agent_logic::clock::now_secs_i64())?),
    }
}

/// `row` is this agent key's valid signature over an entry of its network and party.
fn signed_by_agent(ctx: &ControlContext, row: &SignedEntry) -> bool {
    let entry = row.entry();
    let key = ctx.signer.public_key();
    entry.network() == ctx.network
        && entry.agent_party() == &ctx.party
        && *entry.agent_key() == key
        && verify_ed25519_strict(&key, row.bytes(), row.signature()).is_ok()
}

/// What the rpc stores at the seq of `entry`.
enum Held {
    /// The same entry, validly signed.
    Same,
    /// Another entry validly signed by the agent key: a fork.
    Other,
    Not,
}

fn held_at(ctx: &ControlContext, remote: &RemoteLog, entry: &SignedEntry) -> Held {
    for (seq, bytes, signature) in remote.rows().filter(|(seq, ..)| *seq == entry.seq()) {
        let Ok(row) = SignedEntry::new(bytes.to_vec(), *signature) else {
            continue;
        };
        if row.seq() != seq {
            continue;
        }
        if row.hash() == entry.hash() && (row.signature() == entry.signature() || signed_by_agent(ctx, &row)) {
            return Held::Same;
        }
        if row.hash() != entry.hash() && signed_by_agent(ctx, &row) {
            return Held::Other;
        }
    }
    Held::Not
}

/// Reads the remote log as this agent's: its verified prefix, and where it breaks.
async fn fetch_verified(ctx: &ControlContext, rpc: &ControlRpc) -> Result<(RemoteLog, RemoteView)> {
    let remote = rpc
        .fetch_log()
        .await
        .map_err(|e| anyhow!("cannot compare with the rpc's control log, so nothing is signed: {e}"))?;
    let view = verify_remote(ctx.network, &ctx.party, &ctx.signer.public_key(), &remote).map_err(|e| {
        let message = format!("the rpc's control log for this agent is invalid: {e:#}");
        report_critical(ctx, "agent_control_remote_invalid", &message);
        anyhow!("refusing to continue: {message}")
    })?;
    if let Some(at) = view.broken_at {
        tracing::warn!("agent control: the rpc's control log is broken at seq {at}");
    }
    Ok((remote, view))
}

/// Compares the logs under the lock: what the rpc lacks or holds broken is uploaded, a
/// pending entry settled, and a log ahead or forked refuses. Keeps the lock.
pub async fn prepare_change<W: Write + ?Sized>(ctx: &ControlContext, rpc: &ControlRpc, out: &mut W) -> Result<Prepared> {
    let (remote_head, uploaded) = {
        let lock = ctx.store.lock().await?;
        let (_, remote_head, uploaded) = settle(ctx, rpc, &lock, out).await?;
        drop(lock);
        (remote_head, uploaded)
    };
    // Forward only; the reload takes the lock itself.
    ctx.guard.reload_from(&ctx.store, ctx.marker.as_ref()).await.context("reloading the control state")?;
    let lock = ctx.store.lock().await?;
    let stored = ctx.store.load(&lock)?;
    if let Some(pending) = stored.as_ref().and_then(|s| s.pending.as_ref()) {
        bail!("a pending control entry at seq {} appeared meanwhile: run `cloud-agent control sync`", pending.entry.seq());
    }
    let state = state_of(ctx, stored.as_ref())?;
    ensure!(
        ctx.guard.head() == state.head(),
        "the control state in force (head {}) differs from the store (head {}): retry",
        head_text(ctx.guard.head()),
        head_text(state.head())
    );
    Ok(Prepared { lock, state, remote_head, uploaded })
}

/// The locked part of [`prepare_change`]: (local head moved, remote head, entries uploaded).
async fn settle<W: Write + ?Sized>(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    lock: &ControlLock,
    out: &mut W,
) -> Result<(bool, Option<HeadRef>, usize)> {
    let mut stored = ctx.store.load(lock)?;
    let (remote, view) = fetch_verified(ctx, rpc).await?;
    let mut moved = false;

    // A pending entry the rpc already holds was accepted; a refused one blocks its seq.
    let pending = match &stored {
        Some(s) => s.pending.clone(),
        None => ctx.store.pending(lock)?,
    };
    if let Some(pending) = &pending {
        if pending.status == PendingStatus::Refused {
            bail!(
                "the rpc refused the signed control entry at seq {}{}; it is kept and seq {} is not signed \
                 again automatically: run `cloud-agent control discard-pending` to release it",
                pending.entry.seq(),
                pending.refused_code.map(|c| format!(" (code {c})")).unwrap_or_default(),
                pending.entry.seq()
            );
        }
        match held_at(ctx, &remote, &pending.entry) {
            Held::Same => {
                accept_pending_locked(ctx, lock, pending, out)?;
                stored = ctx.store.load(lock)?;
                moved = true;
            }
            Held::Other => {
                let message = format!(
                    "the rpc holds a different entry at the pending seq {}: the agent key signed two entries there",
                    pending.entry.seq()
                );
                report_critical(ctx, "agent_control_fork", &message);
                bail!("refusing to continue: {message}");
            }
            Held::Not => {}
        }
    }

    let mut remote_head = view.head();
    let mut uploaded = 0usize;
    match &stored {
        None => {
            if let Some(head) = view.head() {
                bail!(
                    "the rpc holds a control log for this agent ({}) but there is no local copy: run \
                     `cloud-agent control restore`",
                    head_text(Some(head))
                );
            }
            let signed = remote
                .rows()
                .filter_map(|(_, bytes, signature)| SignedEntry::new(bytes.to_vec(), *signature).ok())
                .any(|row| signed_by_agent(ctx, &row));
            if signed {
                let message = "the rpc holds control entries this key signed past a broken seq, and there is no local copy";
                report_critical(ctx, "agent_control_remote_invalid", message);
                bail!("refusing to continue: {message}. Nothing is signed");
            }
            if !remote.is_empty() {
                tracing::warn!("agent control: the rpc holds control rows for this agent that do not verify; ignoring them");
            }
        }
        Some(stored) => match standing(&stored.log, &remote, &view)? {
            Standing::Equal => {
                uploaded = upload_missing(ctx, rpc, lock, stored, &remote, &[]).await?;
            }
            Standing::Behind { bad } => {
                let local = stored.log.head();
                let count = bad.len();
                let _ = match (remote.is_empty(), view.broken_at, bad.first()) {
                    (true, _, _) => writeln!(out, "The rpc holds no control log; uploading {count} entries."),
                    (false, Some(_), Some(from)) => writeln!(
                        out,
                        "The rpc's control log is broken at seq {from} (rows missing or not verifying); \
                         repairing it from the local copy, uploading {count} entries."
                    ),
                    _ => writeln!(
                        out,
                        "The rpc's control log is behind ({} < {}); uploading {count} entries.",
                        head_text(view.head()),
                        head_text(Some(local))
                    ),
                };
                uploaded = upload_missing(ctx, rpc, lock, stored, &remote, &bad).await?;
                remote_head = Some(local);
            }
            Standing::Ahead { local, remote_tip, signed } => {
                let message = format!(
                    "the rpc's control log is ahead of the local copy: rpc seq {remote_tip}, local {}{}",
                    head_text(Some(local)),
                    if signed { "" } else { " (its rows past the local head are not validly signed)" }
                );
                report_critical(ctx, "agent_control_remote_ahead", &message);
                bail!("refusing to continue: {message}. Nothing is signed; check which copy is genuine");
            }
            Standing::Fork(evidence) => {
                let message = format!(
                    "agent control fork at seq {}: rpc {}, local {}",
                    evidence.seq(),
                    head_text(Some(head_of(evidence.remote()))),
                    head_text(Some(stored.log.head()))
                );
                report_critical(ctx, "agent_control_fork", &message);
                bail!("refusing to continue: {message}. Nothing is signed");
            }
        },
    }

    // A pending entry the rpc does not hold is uploaded again, byte for byte.
    if let (Some(pending), false) = (&pending, moved) {
        upload_pending(ctx, rpc, lock, pending, out).await?;
        moved = true;
    }
    Ok((moved, if moved { ctx.store.load(lock)?.map(|s| s.log.head()) } else { remote_head }, uploaded))
}

/// The lock again after a confirmation; the store must still be at `expected` with nothing pending.
pub async fn relock(ctx: &ControlContext, expected: Option<HeadRef>) -> Result<(ControlLock, ControlState)> {
    let lock = ctx.store.lock().await?;
    let stored = ctx.store.load(&lock)?;
    ctx.store.check_can_sign(&lock)?;
    let state = state_of(ctx, stored.as_ref())?;
    ensure!(
        state.head() == expected,
        "the control log changed while waiting for the confirmation (now {}): run the command again",
        head_text(state.head())
    );
    Ok((lock, state))
}

/// Signs `entry` as pending, uploads it, and appends it once the rpc accepts it.
pub async fn sign_and_upload(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    lock: ControlLock,
    entry: &PendingEntry,
    out: &mut dyn Write,
) -> Result<HeadRef> {
    let checked = rpc.checked_now()?;
    ensure!(
        checked.unix_secs().abs_diff(entry.entry().created_at().unix_secs())
            <= super::rpc::MAX_CLOCK_SKEW_SECS.unsigned_abs(),
        "refusing to sign: the entry's time is stale; run the command again"
    );
    let now = checked.unix_secs();
    let signed = ctx.store.sign_entry(&lock, &ctx.signer, entry, now)?;
    let documents: Vec<Vec<u8>> = entry.policy_document().map(<[u8]>::to_vec).into_iter().collect();
    let expected = head_of(&signed);
    match rpc.append_retrying(&ctx.store, &lock, std::slice::from_ref(&signed), &documents).await {
        Ok(head) if head == expected => {}
        Ok(head) => bail!(
            "the rpc reports head {} instead of the signed entry at seq {}; it stays pending: run \
             `cloud-agent control sync`",
            head_text(Some(head)),
            expected.seq
        ),
        Err(e) => return Err(pending_refused(ctx, &lock, &e, expected.seq, now)?),
    }
    let head = super::accept_and_mark_locked(&ctx.store, lock, &expected, &ctx.guard, ctx.require_env_file()?).await?;
    let _ = writeln!(out, "Accepted by the rpc: control log head {}.", head_text(Some(head)));
    Ok(head)
}

/// `control sync`: compare with the rpc, upload what it lacks, resolve a pending entry.
pub async fn run_sync<W: Write + ?Sized>(ctx: &ControlContext, rpc: &ControlRpc, out: &mut W) -> Result<()> {
    let prepared = prepare_change(ctx, rpc, out).await?;
    let (head, remote, uploaded) = (prepared.head(), prepared.remote_head, prepared.uploaded);
    drop(prepared);
    let _ = writeln!(
        out,
        "In sync: local {}, rpc {}{}.",
        head_text(head),
        head_text(remote),
        if uploaded > 0 { format!(", {uploaded} entries uploaded") } else { String::new() }
    );
    Ok(())
}

/// `control restore`: rebuilds the local copy from the rpc's verified log. A remote
/// log behind the marker, diverging from it or from the local copy is refused.
pub async fn run_restore(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
) -> Result<()> {
    let lock = ctx.store.lock().await?;
    let (remote, view) = fetch_verified(ctx, rpc).await?;
    let remote_log = view
        .prefix
        .clone()
        .ok_or_else(|| anyhow!("the rpc holds no valid control log for this agent: nothing to restore"))?;
    let docs = remote.policy_documents();
    replay(&remote_log, &docs).map_err(|e| {
        anyhow!("the rpc's control log does not replay at seq {} ({}): it cannot be restored", e.seq, e.code_name())
    })?;
    if let Some(marker) = &ctx.marker {
        marker.check(&remote_log).context("refusing to restore")?;
    }
    let high_water = read_high_water(ctx)?;
    if let Some(head) = high_water {
        ControlMarker { network: ctx.network, head }.check(&remote_log).context("refusing to restore below head.json")?;
    }
    if let Some(at) = view.broken_at {
        bail!(
            "refusing to restore: the rpc's control log is broken at seq {at} (it verifies only through seq {}); \
             a restore needs its whole log. Repair it with `cloud-agent control sync` where the local copy \
             survives, or ask the rpc operator",
            remote_log.head().seq
        );
    }
    let local = match ctx.store.load(&lock) {
        Ok(local) => local,
        Err(e) if high_water.is_some() && !ctx.store.dir().join(LOG_FILE).exists() => {
            tracing::warn!("agent control: restoring over {}: {e:#}", HEAD_FILE);
            None
        }
        Err(e) => return Err(e.context("the local control store does not load; move it aside before a restore")),
    };
    let start = match &local {
        None => 0usize,
        Some(stored) => match compare_logs(&stored.log, &remote_log).map_err(|e| anyhow!("{}", e.code_name()))? {
            LogComparison::Equal => {
                let _ = writeln!(out, "The local copy already equals the rpc's log ({}).", head_text(Some(remote_log.head())));
                return Ok(());
            }
            LogComparison::RemoteAhead { local, .. } => usize::try_from(local.seq).unwrap_or(usize::MAX),
            LogComparison::RemoteBehind { local, remote } => bail!(
                "refusing to restore: the rpc's log ({}) is behind the local copy ({}); run `cloud-agent control sync`",
                head_text(Some(remote)),
                head_text(Some(local))
            ),
            LogComparison::Fork(evidence) => {
                let message = format!("agent control fork at seq {} found during restore", evidence.seq());
                report_critical(ctx, "agent_control_fork", &message);
                bail!("refusing to restore: {message}")
            }
        },
    };
    let pin = ctx.marker.map(|m| m.head).or(high_water).unwrap_or(remote_log.head());
    let _ = writeln!(out, "Restore the control log of {} on {} from the rpc:", ctx.party, ctx.network);
    let _ = writeln!(out, "  rpc head:   {}", head_text(Some(remote_log.head())));
    let _ = writeln!(out, "  local head: {}", head_text(local.as_ref().map(|s| s.log.head())));
    let _ = writeln!(out, "  confirming: {}", head_text(Some(pin)));
    let hash8: String = pin.hash.to_string().chars().take(8).collect();
    let phrase = format!("restore {}:{hash8}", pin.seq);
    confirm_typed(prompter, out, &phrase, Yes::NotGiven).await?;
    if local.is_none() && high_water.is_some() {
        set_aside(ctx, HEAD_FILE)?;
    }
    let tail: Vec<SignedEntry> = remote_log.entries().iter().skip(start).cloned().collect();
    let documents: Vec<Vec<u8>> = remote.documents.clone();
    let head = ctx.store.append(&lock, &tail, &documents)?;
    let marked = ctx.mark(head, out);
    drop(lock);
    marked?;
    ctx.guard.reload_from(&ctx.store, None).await.context("reloading the restored control state")?;
    let _ = writeln!(out, "Restored: control log head {}.", head_text(Some(head)));
    Ok(())
}

/// head.json's head, read directly when log.json is missing.
fn read_high_water(ctx: &ControlContext) -> Result<Option<HeadRef>> {
    let path = ctx.store.dir().join(HEAD_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let value: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("{} is not JSON", path.display()))?;
    let seq = value.get("seq").and_then(serde_json::Value::as_u64);
    let hash = value.get("hash").and_then(serde_json::Value::as_str).and_then(|h| agent_control::Hash32::parse(h).ok());
    match (seq, hash) {
        (Some(seq), Some(hash)) if seq >= 1 => Ok(Some(HeadRef { seq, hash })),
        _ => bail!("{} holds no valid head", path.display()),
    }
}

/// Renames a store file to `<name>.<unix secs>.bak`, keeping it.
fn set_aside(ctx: &ControlContext, name: &str) -> Result<()> {
    let from = ctx.store.dir().join(name);
    let to = ctx.store.dir().join(format!("{name}.{}.bak", agent_logic::clock::now_secs()));
    std::fs::rename(&from, &to).with_context(|| format!("move {} aside", from.display()))
}

/// Writes `bytes` to `path` (0600), synced, without replacing an existing file.
fn write_new_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all().with_context(|| format!("sync {}", path.display()))
}

/// `control discard-pending`: releases a pending entry the rpc does not hold. A copy is
/// kept under evidence/, because the signed entry still exists.
pub async fn run_discard_pending(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
) -> Result<()> {
    let lock = ctx.store.lock().await?;
    let pending = ctx.store.pending(&lock)?.ok_or_else(|| anyhow!("no pending control entry"))?;
    let seq = pending.entry.seq();
    match rpc.fetch_log().await {
        Ok(remote) => {
            let held = remote.rows().find(|(_, bytes, _)| *bytes == pending.entry.bytes());
            ensure!(
                held.is_none(),
                "the rpc holds the pending entry at seq {seq}: it was accepted; run `cloud-agent control sync`"
            );
        }
        Err(e) if pending.status == PendingStatus::Refused => {
            tracing::warn!("agent control: discarding a refused entry without checking the rpc: {e}");
        }
        Err(e) => bail!("cannot check whether the rpc holds the pending entry ({e}); retry when it is reachable"),
    }
    let status = match pending.status {
        PendingStatus::Uploading => "awaiting the rpc's answer".to_string(),
        PendingStatus::Refused => format!(
            "refused by the rpc{}",
            pending.refused_code.map(|c| format!(" (code {c})")).unwrap_or_default()
        ),
    };
    let _ = writeln!(out, "Pending control entry at seq {seq}: {} ({status}).", pending.entry.kind().as_str());
    let _ = writeln!(
        out,
        "Warning: this entry is signed by the agent key. If anyone kept a copy, signing a different entry \
         at seq {seq} later lets them show two agent signatures at one seq, which reads as a fork. A copy \
         is kept under {EVIDENCE_DIR}/."
    );
    confirm_typed(prompter, out, &format!("discard {seq}"), Yes::Refused).await?;
    let hash8: String = pending.entry.hash().to_string().chars().take(8).collect();
    let evidence = serde_json::json!({
        "seq": seq,
        "entry_hash": pending.entry.hash().to_string(),
        "canonical_b64": BASE64.encode(pending.entry.bytes()),
        "signature_b64": BASE64.encode(pending.entry.signature()),
        "status": status,
        "discarded_at": agent_logic::clock::now_secs_i64(),
    });
    let path = ctx.store.dir().join(EVIDENCE_DIR).join(format!("discarded-{seq}-{hash8}.json"));
    write_new_private(&path, serde_json::to_string_pretty(&evidence)?.as_bytes())?;
    let file = ctx.store.dir().join(PENDING_FILE);
    std::fs::remove_file(&file).with_context(|| format!("remove {}", file.display()))?;
    if let Ok(dir) = std::fs::File::open(ctx.store.dir()) {
        let _ = dir.sync_all();
    }
    drop(lock);
    let _ = writeln!(out, "Discarded; the copy is in {}.", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::reverified_tip;

    #[test]
    fn a_partial_repair_reaches_the_next_bad_seq_the_rpc_tip_or_the_local_head() {
        // One edited row of 200: the rpc verifies every row above it again.
        assert_eq!(reverified_tip(&[50], 50, 200, 200), 200);
        // Two bad regions: the first stops below the second, which reaches the tip.
        let bad = [40, 41, 42, 150, 151];
        assert_eq!(reverified_tip(&bad, 42, 200, 200), 149);
        assert_eq!(reverified_tip(&bad, 151, 200, 200), 200);
        // A region past one request: a chunk followed by a bad seq stops at its own end.
        let long: Vec<u64> = (50..=129).collect();
        assert_eq!(reverified_tip(&long, 113, 200, 200), 113);
        assert_eq!(reverified_tip(&long, 129, 200, 200), 200);
        // Capped at the rpc's tip and the local head, never below the upload itself.
        assert_eq!(reverified_tip(&[5], 5, 81, 90), 81);
        assert_eq!(reverified_tip(&[5], 5, 95, 90), 90);
        assert_eq!(reverified_tip(&[50, 151, 152], 152, 150, 152), 152);
        assert_eq!(reverified_tip(&[], 7, 0, 7), 7);
    }
}
