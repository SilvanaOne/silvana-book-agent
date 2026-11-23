//! The daemon's control poller: while the agent is linked, every 60 s it settles the pending
//! policies, then hands the pending commands to the executor, oldest first.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_logic::config::BaseConfig;
use agent_logic::shutdown::Shutdown;
use agent_logic::signer::{ControlGuard, Reload};
use agent_logic::supervise::{self, Policy};
use anyhow::{Result, anyhow};
use orderbook_proto::orderbook::{ListPendingControlResponse, PendingCommand};
use tracing::{debug, info, warn};

use super::ControlContext;
use super::policy_accept::{self, Candidate, Outcome};
use super::rpc::{ControlRpc, RpcError};

/// Time between two rounds.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Bound on re-reading the local control log.
pub const RELOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound on one policy: the log comparison, then the acceptance or the refusal.
pub const POLICY_TIMEOUT: Duration = Duration::from_secs(180);
/// Bound on one command in the executor.
pub const EXECUTE_TIMEOUT: Duration = Duration::from_secs(1200);
/// Policies taken in one round.
const MAX_POLICIES_PER_ROUND: usize = 8;
/// Commands taken in one round.
const MAX_COMMANDS_PER_ROUND: usize = 16;
/// Names remembered so that an item still waiting is logged once.
const MAX_REMEMBERED: usize = 1024;

/// Runs one pending direct command for the poller.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait CommandExecutor: Send + Sync {
    /// Called one command at a time, oldest first, after the local log was re-read.
    async fn execute(
        &self,
        ctx: &ControlContext,
        rpc: &ControlRpc,
        command: &PendingCommand,
    ) -> Result<()>;

    /// Called once per round after the listed commands: settles the local records the
    /// listing does not name.
    async fn settle_open(
        &self,
        _ctx: &ControlContext,
        _rpc: &ControlRpc,
        _listed: &[PendingCommand],
    ) -> Result<()> {
        Ok(())
    }
}

/// Inserts `name`; true when it was not remembered yet. The set is bounded.
fn remember(set: &Mutex<BTreeSet<String>>, name: &str) -> bool {
    let mut set = agent_logic::sync::lock(set);
    if set.len() >= MAX_REMEMBERED {
        set.clear();
    }
    set.insert(name.to_string())
}

/// The executor of an agent that does not run commands itself: they wait for the CLI.
#[derive(Debug, Default)]
pub struct WaitingExecutor {
    announced: Mutex<BTreeSet<String>>,
}

#[async_trait::async_trait]
impl CommandExecutor for WaitingExecutor {
    async fn execute(
        &self,
        _ctx: &ControlContext,
        _rpc: &ControlRpc,
        command: &PendingCommand,
    ) -> Result<()> {
        if remember(&self.announced, &command.command_id) {
            info!(
                "agent control: command {} is waiting for control execute (cloud-agent control execute --code CMD-{})",
                command.command_id, command.command_id
            );
        }
        Ok(())
    }
}

/// What one round did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub accepted: u32,
    pub refused: u32,
    pub left: u32,
    pub waiting: u32,
    pub handed: u32,
    /// Commands not run because a listed policy was not settled.
    pub held: u32,
    pub failed: u32,
}

impl Report {
    fn any(&self) -> bool {
        *self != Self::default()
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "policies accepted={} refused={} left={} waiting={}; commands handed={} held={}; failed={}",
            self.accepted,
            self.refused,
            self.left,
            self.waiting,
            self.handed,
            self.held,
            self.failed
        )
    }
}

fn bump(counter: &mut u32) {
    *counter = counter.saturating_add(1);
}

/// What the poller keeps between rounds.
#[derive(Debug, Default)]
pub struct Memo {
    noted: Mutex<BTreeSet<String>>,
}

impl Memo {
    /// True the first time `name` is seen.
    fn first(&self, name: &str) -> bool {
        remember(&self.noted, name)
    }
}

/// When the rpc saw an item, oldest first; an item without a time goes last.
fn submitted_key(at: Option<&prost_types::Timestamp>) -> (i64, i32) {
    at.map_or((i64::MAX, i32::MAX), |t| (t.seconds, t.nanos))
}

/// Re-reads the control log under its lock, forward only; false while not linked. An
/// agent with no control store is never touched. A linked one takes unset pins from its env file.
pub async fn reread(ctx: &ControlContext) -> Result<bool> {
    if !ctx.guard.is_linked() && !ctx.store.exists() {
        return Ok(false);
    }
    match tokio::time::timeout(
        RELOAD_TIMEOUT,
        ctx.guard.reload_from(&ctx.store, ctx.marker.as_ref()),
    )
    .await
    {
        Ok(reloaded) => {
            let linked_now = matches!(reloaded?, Reload::Advanced { from: None, .. });
            let linked = ctx.guard.is_linked();
            if linked {
                ctx.pick_up_pins(linked_now);
            }
            Ok(linked)
        }
        Err(_) => Err(anyhow!(
            "the control log was not re-read within {RELOAD_TIMEOUT:?}"
        )),
    }
}

/// Logs what a step wrote for the terminal.
fn log_account(out: &[u8]) {
    for line in String::from_utf8_lossy(out)
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        info!("agent control: {line}");
    }
}

/// Runs the future `make` builds on a blocking thread of the current runtime, so that it
/// may hold a writer that cannot move between threads.
pub async fn on_blocking_thread<T, F, Fut>(make: F) -> Result<T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T>,
    T: Send + 'static,
{
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|e| anyhow!("the step has no runtime: {e}"))?;
    let runner = handle.clone();
    handle
        .spawn_blocking(move || runner.block_on(make()))
        .await
        .map_err(|e| anyhow!("the step stopped: {e}"))
}

/// One pending policy, accepted or refused within [`POLICY_TIMEOUT`]; with its account.
async fn settle_policy(
    ctx: &Arc<ControlContext>,
    rpc: &ControlRpc,
    candidate: &Candidate,
) -> (Result<Outcome>, Vec<u8>) {
    let (ctx, rpc, candidate) = (Arc::clone(ctx), rpc.clone(), candidate.clone());
    let step = on_blocking_thread(move || async move {
        let mut out = Vec::new();
        let result = tokio::time::timeout(
            POLICY_TIMEOUT,
            policy_accept::accept_automatically(&ctx, &rpc, &candidate, &mut out),
        )
        .await;
        let result =
            result.unwrap_or_else(|_| Err(anyhow!("it was not settled within {POLICY_TIMEOUT:?}")));
        (result, out)
    })
    .await;
    step.unwrap_or_else(|e| (Err(e), Vec::new()))
}

/// One listing: the policies first, then the commands, each bounded.
pub async fn handle(
    ctx: &Arc<ControlContext>,
    rpc: &ControlRpc,
    listing: ListPendingControlResponse,
    executor: &dyn CommandExecutor,
    memo: &Memo,
) -> Report {
    let mut report = Report::default();
    let mut policies = listing.policies;
    policies.sort_by(|a, b| {
        submitted_key(a.submitted_at.as_ref())
            .cmp(&submitted_key(b.submitted_at.as_ref()))
            .then_with(|| a.policy_hash.cmp(&b.policy_hash))
    });
    let waits = if !ctx.config.control.auto_accept_policies {
        Some("[control] auto_accept_policies = false")
    } else if ctx.env_file.is_none() {
        Some("no env file is known for the marker")
    } else {
        None
    };
    // A command signed at the current head goes stale once a listed policy is accepted.
    let mut unsettled = policies.len() > MAX_POLICIES_PER_ROUND;
    for item in policies.iter().take(MAX_POLICIES_PER_ROUND) {
        if let Some(reason) = waits {
            if memo.first(&item.policy_hash) {
                info!(
                    "agent control: policy {} waits for `cloud-agent control policy accept` ({reason})",
                    item.policy_hash
                );
            }
            bump(&mut report.waiting);
            unsettled = true;
            continue;
        }
        let candidate = match Candidate::from_pending(item) {
            Ok(candidate) => candidate,
            Err(e) => {
                warn!("agent control: {e:#}");
                bump(&mut report.failed);
                unsettled = true;
                continue;
            }
        };
        let (step, out) = settle_policy(ctx, rpc, &candidate).await;
        log_account(&out);
        match step {
            Ok(Outcome::Accepted(head)) => {
                info!(
                    "agent control: accepted policy {} at seq {}",
                    candidate.hash, head.seq
                );
                bump(&mut report.accepted);
            }
            Ok(Outcome::Refused(code)) => {
                warn!(
                    "agent control: refused policy {} (code {code}) with a signed receipt",
                    candidate.hash
                );
                bump(&mut report.refused);
            }
            Ok(Outcome::AlreadyAccepted) => {
                debug!(
                    "agent control: policy {} is already accepted; the listing lags",
                    candidate.hash
                );
            }
            Ok(Outcome::Left) => {
                if memo.first(&item.policy_hash) {
                    info!(
                        "agent control: policy {} is issued later than this clock allows; left pending",
                        candidate.hash
                    );
                }
                bump(&mut report.left);
                unsettled = true;
            }
            Err(e) => {
                warn!("agent control: policy {}: {e:#}", candidate.hash);
                bump(&mut report.failed);
                unsettled = true;
                break;
            }
        }
    }

    let mut commands = listing.commands;
    commands.sort_by(|a, b| {
        submitted_key(a.submitted_at.as_ref())
            .cmp(&submitted_key(b.submitted_at.as_ref()))
            .then_with(|| a.command_id.cmp(&b.command_id))
    });
    if unsettled {
        for command in commands.iter().take(MAX_COMMANDS_PER_ROUND) {
            if memo.first(&format!("held {}", command.command_id)) {
                info!(
                    "agent control: command {} waits until the listed policies are settled",
                    command.command_id
                );
            }
            bump(&mut report.held);
        }
    }
    let run = if unsettled { 0 } else { MAX_COMMANDS_PER_ROUND };
    let mut linked = true;
    for command in commands.iter().take(run) {
        match reread(ctx).await {
            Ok(true) => {}
            Ok(false) => {
                linked = false;
                break;
            }
            Err(e) => {
                warn!("agent control: no command is run: {e:#}");
                bump(&mut report.failed);
                linked = false;
                break;
            }
        }
        match tokio::time::timeout(EXECUTE_TIMEOUT, executor.execute(ctx, rpc, command)).await {
            Ok(Ok(())) => bump(&mut report.handed),
            Ok(Err(e)) => {
                warn!("agent control: command {}: {e:#}", command.command_id);
                bump(&mut report.failed);
            }
            Err(_) => {
                warn!(
                    "agent control: command {} did not finish within {EXECUTE_TIMEOUT:?}",
                    command.command_id
                );
                bump(&mut report.failed);
            }
        }
    }
    if linked && ctx.guard.is_linked() {
        let listed: &[PendingCommand] = if unsettled { &[] } else { &commands };
        match tokio::time::timeout(EXECUTE_TIMEOUT, executor.settle_open(ctx, rpc, listed)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warn!("agent control: open commands were not settled: {e:#}");
                bump(&mut report.failed);
            }
            Err(_) => {
                warn!("agent control: open commands were not settled within {EXECUTE_TIMEOUT:?}");
                bump(&mut report.failed);
            }
        }
    }
    report
}

/// One round: the re-read, the listing, then [`handle`]. Errors are logged, never returned.
async fn round(
    ctx: &Arc<ControlContext>,
    rpc: &mut Option<ControlRpc>,
    executor: &dyn CommandExecutor,
    memo: &Memo,
) {
    match reread(ctx).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            warn!("agent control: the poller skips a round: {e:#}");
            return;
        }
    }
    let client = match rpc {
        Some(client) => client.clone(),
        None => match ControlRpc::connect(&ctx.config).await {
            Ok(client) => {
                *rpc = Some(client.clone());
                client
            }
            Err(e) => {
                warn!("agent control: the poller skips a round: {e}");
                return;
            }
        },
    };
    let listing = match client.pending().await {
        Ok(listing) => listing,
        Err(RpcError::Unimplemented(_)) => {
            if memo.first("unimplemented") {
                warn!("agent control: the rpc lists no pending control work yet");
            }
            return;
        }
        Err(e) => {
            warn!("agent control: listing pending control work failed: {e}");
            if matches!(e, RpcError::Unreachable(_)) {
                *rpc = None;
            }
            return;
        }
    };
    let report = handle(ctx, &client, listing, executor, memo).await;
    if report.any() {
        info!("agent control: poller round: {report}");
    }
}

async fn poll_loop(
    ctx: Arc<ControlContext>,
    executor: Arc<dyn CommandExecutor>,
    shutdown: Shutdown,
) {
    let memo = Memo::default();
    let mut rpc: Option<ControlRpc> = None;
    loop {
        if shutdown.sleep(POLL_INTERVAL).await {
            return;
        }
        round(&ctx, &mut rpc, executor.as_ref(), &memo).await;
    }
}

/// Logs the control settings and starts the poller on a control network; it acts only
/// while the agent is linked, and never under a dry run.
pub fn spawn(
    config: &BaseConfig,
    guard: Arc<ControlGuard>,
    env_file: Option<PathBuf>,
    dry_run: bool,
    shutdown: Shutdown,
    executor: Arc<dyn CommandExecutor>,
) {
    info!(
        "agent control settings: {}; vetted_packages: {} (not enforced yet)",
        config.control.describe(),
        config.vetted_packages.describe()
    );
    if dry_run {
        info!("agent control: the poller does not run under --dry-run");
        return;
    }
    let linked = guard.is_linked();
    let ctx = match ControlContext::for_daemon(config.clone(), guard, env_file) {
        Ok(Some(ctx)) => Arc::new(ctx),
        Ok(None) => {
            debug!("agent control: CANTON_CHAIN is not a control network; no poller");
            return;
        }
        Err(e) if linked => {
            warn!("agent control: the poller is not started: {e:#}");
            return;
        }
        Err(e) => {
            debug!("agent control: no poller for this unlinked agent: {e:#}");
            return;
        }
    };
    let task_shutdown = shutdown.clone();
    let spawned =
        supervise::spawn_supervised("control poller", shutdown, Policy::Restart, move || {
            poll_loop(
                Arc::clone(&ctx),
                Arc::clone(&executor),
                task_shutdown.clone(),
            )
        });
    if let Err(e) = spawned {
        warn!("agent control: the poller is not started: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::policy_accept::tests::{LINKED, candidate, now};
    use crate::control::test_support::{FakeRpc, TestAgent};
    use agent_control::{DEFAULT_LINKED_RULES, Hash32, Rules};
    use orderbook_proto::orderbook::PendingPolicy;

    /// Records the commands it is handed, in order.
    #[derive(Default)]
    struct Recording {
        seen: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl CommandExecutor for Recording {
        async fn execute(
            &self,
            _ctx: &ControlContext,
            _rpc: &ControlRpc,
            command: &PendingCommand,
        ) -> Result<()> {
            agent_logic::sync::lock(&self.seen).push(command.command_id.clone());
            Ok(())
        }
    }

    fn at(seconds: i64) -> Option<prost_types::Timestamp> {
        Some(prost_types::Timestamp { seconds, nanos: 0 })
    }

    fn listed(policy: &Candidate, submitted: i64) -> PendingPolicy {
        PendingPolicy {
            policy_hash: policy.hash.to_string(),
            document: policy.document.clone(),
            controller_id: String::new(),
            proof: policy.proof.clone(),
            submitted_at: at(submitted),
        }
    }

    fn command(id: &str, submitted: Option<prost_types::Timestamp>) -> PendingCommand {
        PendingCommand {
            command_id: id.into(),
            submitted_at: submitted,
            ..Default::default()
        }
    }

    async fn context(agent: &TestAgent) -> (Arc<ControlContext>, ControlRpc) {
        let ctx = Arc::new(agent.context().await);
        let rpc = ControlRpc::connect(&ctx.config).await.unwrap();
        (ctx, rpc)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn policies_are_settled_first_then_commands_go_out_oldest_first() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0x64; 32], &rpc, "poller-order");
        agent.linked(&rpc, 1).await;
        let (ctx, client) = context(&agent).await;
        let paused = Rules {
            paused: true,
            ..DEFAULT_LINKED_RULES
        };
        let v1 = candidate(
            &agent.party,
            1,
            Hash32([0; 32]),
            paused,
            LINKED,
            now(),
            3600,
        );
        let rival = candidate(
            &agent.party,
            1,
            Hash32([0; 32]),
            DEFAULT_LINKED_RULES,
            LINKED,
            now(),
            3600,
        );
        let listing = ListPendingControlResponse {
            policies: vec![listed(&rival, 200), listed(&v1, 100)],
            commands: vec![
                command("b", at(300)),
                command("c", None),
                command("a", at(100)),
            ],
            ..Default::default()
        };
        rpc.state()
            .policies
            .insert(rival.hash.to_string(), agent.party.clone());
        let executor = Recording::default();
        let report = handle(&ctx, &client, listing, &executor, &Memo::default()).await;
        // The older policy is accepted; the rival is then superseded, and its signed refusal
        // passes the rpc's checks.
        assert_eq!(
            (report.accepted, report.refused, report.failed),
            (1, 1, 0),
            "{report}"
        );
        let acks = rpc.state().acks.clone();
        assert_eq!(
            acks.iter()
                .map(|(hash, _)| hash.clone())
                .collect::<Vec<_>>(),
            vec![rival.hash.to_string()]
        );
        assert_eq!(rpc.seqs(), vec![1, 2]);
        assert_eq!(ctx.guard.head().map(|h| h.seq), Some(2));
        assert_eq!(
            *agent_logic::sync::lock(&executor.seen),
            vec!["a", "b", "c"]
        );
        assert_eq!(report.handed, 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn with_auto_accept_off_policies_wait_and_nothing_is_signed() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0x65; 32], &rpc, "poller-manual");
        agent.config.control.auto_accept_policies = false;
        agent.linked(&rpc, 1).await;
        let (ctx, client) = context(&agent).await;
        let v1 = candidate(
            &agent.party,
            1,
            Hash32([0; 32]),
            DEFAULT_LINKED_RULES,
            LINKED,
            now(),
            3600,
        );
        let listing = ListPendingControlResponse {
            policies: vec![listed(&v1, 1)],
            ..Default::default()
        };
        let memo = Memo::default();
        let report = handle(
            &ctx,
            &client,
            listing.clone(),
            &WaitingExecutor::default(),
            &memo,
        )
        .await;
        assert_eq!(
            report,
            Report {
                waiting: 1,
                ..Report::default()
            }
        );
        assert_eq!(rpc.seqs(), vec![1]);
        assert!(!memo.first(&v1.hash.to_string()), "logged once");
        // No env file for the marker also leaves them waiting.
        let mut unmarked = agent.context().await;
        unmarked.env_file = None;
        unmarked.config.control.auto_accept_policies = true;
        let report = handle(
            &Arc::new(unmarked),
            &client,
            listing,
            &WaitingExecutor::default(),
            &memo,
        )
        .await;
        assert_eq!(report.waiting, 1);
        assert_eq!(rpc.seqs(), vec![1]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_command_runs_in_a_round_whose_listed_policy_is_not_settled() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0x15; 32], &rpc, "poller-held");
        agent.config.control.auto_accept_policies = false;
        agent.linked(&rpc, 1).await;
        let (ctx, client) = context(&agent).await;
        let v1 = candidate(
            &agent.party,
            1,
            Hash32([0; 32]),
            DEFAULT_LINKED_RULES,
            LINKED,
            now(),
            3600,
        );
        let executor = Recording::default();
        // Waiting for `control policy accept`.
        let listing = ListPendingControlResponse {
            policies: vec![listed(&v1, 1)],
            commands: vec![command("a", at(2))],
            ..Default::default()
        };
        let report = handle(&ctx, &client, listing, &executor, &Memo::default()).await;
        assert_eq!(
            (report.waiting, report.handed, report.held),
            (1, 0, 1),
            "{report}"
        );
        // Not decodable, so not settled either.
        let mut on = agent.context().await;
        on.config.control.auto_accept_policies = true;
        let on = Arc::new(on);
        let garbled = PendingPolicy {
            policy_hash: Hash32([7; 32]).to_string(),
            ..listed(&v1, 1)
        };
        let listing = ListPendingControlResponse {
            policies: vec![garbled],
            commands: vec![command("a", at(2))],
            ..Default::default()
        };
        let report = handle(&on, &client, listing, &executor, &Memo::default()).await;
        assert_eq!(
            (report.failed, report.handed, report.held),
            (1, 0, 1),
            "{report}"
        );
        assert!(agent_logic::sync::lock(&executor.seen).is_empty());
        // With no policy listed, the command goes out.
        let listing = ListPendingControlResponse {
            commands: vec![command("a", at(2))],
            ..Default::default()
        };
        let report = handle(&on, &client, listing, &executor, &Memo::default()).await;
        assert_eq!((report.handed, report.held), (1, 0), "{report}");
        assert_eq!(*agent_logic::sync::lock(&executor.seen), vec!["a"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unlinked_agent_is_never_touched() {
        let rpc = FakeRpc::start().await;
        let agent = TestAgent::new([0x66; 32], &rpc, "poller-unlinked");
        let (ctx, client) = context(&agent).await;
        assert!(!reread(&ctx).await.unwrap());
        let listing = ListPendingControlResponse {
            commands: vec![command("a", at(1))],
            ..Default::default()
        };
        let executor = Recording::default();
        let report = handle(&ctx, &client, listing, &executor, &Memo::default()).await;
        assert_eq!(report, Report::default());
        assert!(agent_logic::sync::lock(&executor.seen).is_empty());
        assert!(
            !ctx.store.exists() && !ctx.store.dir().exists(),
            "no store is created"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_link_at_runtime_takes_the_pins_the_env_file_gained_and_never_replaces_one() {
        let party = |hint: &str, digit: &str| format!("{hint}::1220{}", digit.repeat(64));
        let (kept, other, traffic) = (
            party("kept", "d"),
            party("other", "e"),
            party("traffic", "f"),
        );
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0x67; 32], &rpc, "poller-pins");
        agent.config.party_fee = Some(kept.clone());
        let (ctx, _) = context(&agent).await;
        assert!(!reread(&ctx).await.unwrap());
        // The link lands with what its command wrote to the env file.
        crate::upsert_env_value(&agent.env_file, "PARTY_FEE", &other).unwrap();
        crate::upsert_env_value(&agent.env_file, "PARTY_PREPAID_TRAFFIC", "traffic").unwrap();
        agent.linked(&rpc, 1).await;
        assert!(reread(&ctx).await.unwrap());
        let pins = |traffic: Option<&String>| crate::control::PlatformParties {
            fee: Some(kept.clone()),
            prepaid_traffic: traffic.cloned(),
        };
        assert_eq!(ctx.platform(), pins(None), "kept, and not a party id");
        // A pin written later is taken on the next round, without a restart.
        crate::upsert_env_value(&agent.env_file, "PARTY_PREPAID_TRAFFIC", &traffic).unwrap();
        assert!(reread(&ctx).await.unwrap());
        assert_eq!(ctx.platform(), pins(Some(&traffic)));
        crate::upsert_env_value(&agent.env_file, "PARTY_PREPAID_TRAFFIC", &other).unwrap();
        assert!(reread(&ctx).await.unwrap());
        assert_eq!(ctx.platform(), pins(Some(&traffic)), "never replaced");
    }

    #[test]
    fn remembered_names_are_bounded_and_reported_once() {
        let set = Mutex::new(BTreeSet::new());
        assert!(remember(&set, "x") && !remember(&set, "x"));
        for i in 0..MAX_REMEMBERED {
            remember(&set, &i.to_string());
        }
        assert!(agent_logic::sync::lock(&set).len() <= MAX_REMEMBERED);
        assert_eq!(submitted_key(None), (i64::MAX, i32::MAX));
        assert!(submitted_key(at(5).as_ref()) < submitted_key(None));
        assert!(
            Report::default()
                .to_string()
                .contains("policies accepted=0")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_step_holding_a_terminal_writer_runs_on_a_blocking_thread() {
        let got = on_blocking_thread(|| async {
            let mut out: Vec<u8> = Vec::new();
            let writer: &mut dyn std::io::Write = &mut out;
            tokio::task::yield_now().await;
            let _ = writeln!(writer, "done");
            String::from_utf8(out).unwrap()
        })
        .await
        .unwrap();
        assert_eq!(got, "done\n");
    }
}
