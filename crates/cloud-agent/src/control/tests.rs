use super::*;
use agent_logic::control_store::testing::{TestChain, party_for};
use agent_logic::secret::Secret;
use std::path::PathBuf;

enum Fake {
    Log(RemoteLog),
    Unreachable,
    Refused,
}

#[async_trait::async_trait]
impl LogSource for Fake {
    async fn fetch(&self, _: &BaseConfig, _: &AgentSigner) -> Result<RemoteLog, ProbeError> {
        match self {
            Fake::Log(log) => Ok(log.clone()),
            Fake::Unreachable => Err(ProbeError::Unreachable("Unavailable: connection refused".into())),
            Fake::Refused => Err(ProbeError::Refused("PermissionDenied: request refused (code 4083)".into())),
        }
    }
}

fn remote(entries: &[agent_control::SignedEntry]) -> Fake {
    Fake::Log(RemoteLog {
        entries: entries.iter().map(|e| (e.bytes().to_vec(), *e.signature())).collect(),
        ..Default::default()
    })
}

fn temp_base(tag: &str) -> PathBuf {
    let mut bytes = [0u8; 8];
    let _ = rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    let dir = std::env::temp_dir().join(format!("attach-{tag}-{}", hex::encode(bytes)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An agent of `seed` with a store holding the first `stored` of `built` chain entries.
struct Agent {
    config: BaseConfig,
    base: PathBuf,
    store: ControlStore,
    chain: TestChain,
}

async fn agent(seed: [u8; 32], built: usize, stored: usize, tag: &str) -> Agent {
    let party = party_for(seed, "agent-1").unwrap();
    let mut config = BaseConfig::test_minimal().unwrap();
    config.private_key = Secret::seal(&mut { seed }).unwrap();
    config.party_id = party.to_string();
    config.canton_chain = Some("devnet".into());
    let base = temp_base(tag);
    let store = ControlStore::open(&base, Network::Devnet, &party);
    let chain = TestChain::build(seed, &party, Network::Devnet, built.max(1)).unwrap();
    if stored > 0 {
        let lock = store.lock().await.unwrap();
        store.append(&lock, &chain.entries[..stored], &[]).unwrap();
    }
    Agent { config, base, store, chain }
}

impl Agent {
    fn env(&self, marker: Option<HeadRef>) -> ControlEnv {
        ControlEnv {
            control_dir: Some(self.base.as_os_str().to_owned()),
            home: None,
            chain: Some("devnet".into()),
            marker: marker.map(|head| ControlMarker { network: Network::Devnet, head }.to_string()),
            party: Some(self.config.party_id.clone()),
            agent_dir: Some(self.base.clone()),
        }
    }

    fn head(&self, seq: usize) -> HeadRef {
        HeadRef { seq: seq as u64, hash: *self.chain.entries[seq - 1].hash() }
    }
}

const DAEMON: AttachOptions = AttachOptions { purpose: Purpose::Agent, force: false, no_reject: false };

async fn refusal(a: &Agent, opts: AttachOptions, env: &ControlEnv, source: &Fake) -> String {
    format!("{:#}", attach_with(&a.config, opts, env, source).await.unwrap_err())
}

#[tokio::test]
async fn drill_marker_set_and_control_dir_deleted() {
    let a = agent([0x81; 32], 1, 1, "deleted").await;
    let env = a.env(Some(a.head(1)));
    std::fs::remove_dir_all(a.store.dir()).unwrap();
    for source in [remote(&a.chain.entries), Fake::Unreachable, remote(&[])] {
        let err = refusal(&a, DAEMON, &env, &source).await;
        assert!(err.contains("refusing to start") && err.contains("missing"), "{err}");
    }
}

#[tokio::test]
async fn drill_marker_set_and_rpc_unavailable() {
    let a = agent([0x82; 32], 1, 1, "unavailable").await;
    let err = refusal(&a, DAEMON, &a.env(Some(a.head(1))), &Fake::Unreachable).await;
    assert!(err.contains("cannot be read") && err.contains("Unavailable"), "{err}");
    // The real rpc client classifies a refused connection the same way.
    let mut config = a.config.clone();
    config.orderbook_grpc_url = crate::test_util::refused_url();
    let signer = agent_logic::signer::resolve_with(&config.private_key, &a.env(Some(a.head(1)))).unwrap();
    let probed = RpcLogSource.fetch(&config, &signer).await;
    assert!(matches!(probed, Err(ProbeError::Unreachable(_))), "{probed:?}");
    let err = format!("{:#}", attach_with(&config, DAEMON, &a.env(Some(a.head(1))), &RpcLogSource).await.unwrap_err());
    assert!(err.contains("cannot be read"), "{err}");
    // Without the marker an unreachable rpc does not stop a linked agent.
    let guard = attach_with(&a.config, DAEMON, &a.env(None), &Fake::Unreachable).await.unwrap();
    assert!(guard.is_linked() && guard.strict());
}

#[tokio::test]
async fn drill_marker_set_and_store_behind_it() {
    let a = agent([0x83; 32], 2, 1, "behind").await;
    let err = refusal(&a, DAEMON, &a.env(Some(a.head(2))), &remote(&a.chain.entries)).await;
    assert!(err.contains("behind the linked marker"), "{err}");
    // An unmapped or missing network is refused while the marker is set.
    for chain in [Some("testnet".to_string()), None] {
        let env = ControlEnv { chain: chain.clone(), ..a.env(Some(a.head(1))) };
        assert!(attach_with(&a.config, DAEMON, &env, &remote(&a.chain.entries)).await.is_err(), "{chain:?}");
    }
}

#[tokio::test]
async fn an_rpc_refusal_stops_startup_only_with_the_marker() {
    let unlinked = agent([0x84; 32], 1, 0, "refused-unlinked").await;
    let guard = attach_with(&unlinked.config, DAEMON, &unlinked.env(None), &Fake::Refused).await.unwrap();
    assert!(!guard.is_linked(), "a refusal without the marker continues unlinked");
    let linked = agent([0x8b; 32], 1, 1, "refused-linked").await;
    let guard = attach_with(&linked.config, DAEMON, &linked.env(None), &Fake::Refused).await.unwrap();
    assert!(guard.is_linked(), "without the marker the local log is kept");
    let err = refusal(&linked, DAEMON, &linked.env(Some(linked.head(1))), &Fake::Refused).await;
    assert!(err.contains("refused the control log query") && err.contains("4083"), "{err}");
}

#[tokio::test]
async fn a_remote_log_without_a_local_copy_needs_a_restore() {
    let a = agent([0x85; 32], 2, 0, "restore").await;
    let err = refusal(&a, DAEMON, &a.env(None), &remote(&a.chain.entries)).await;
    assert!(err.contains("control restore"), "{err}");
    // A remote log that does not verify under this key is only a warning without the marker.
    let other = agent([0x8c; 32], 1, 0, "foreign").await;
    let guard = attach_with(&a.config, DAEMON, &a.env(None), &remote(&other.chain.entries)).await.unwrap();
    assert!(!guard.is_linked());
    let mut bad = a.chain.entries[0].bytes().to_vec();
    bad.push(b'x');
    let garbled = Fake::Log(RemoteLog { entries: vec![(bad, *a.chain.entries[0].signature())], ..Default::default() });
    assert!(!attach_with(&a.config, DAEMON, &a.env(None), &garbled).await.unwrap().is_linked());
}

// The attach fix: without the marker no probe failure stops startup.
#[tokio::test]
async fn without_the_marker_every_probe_failure_continues_unlinked() {
    use super::test_support::FakeRpc;
    let fake = FakeRpc::start().await;
    let mut a = agent([0x8d; 32], 1, 0, "probe-statuses").await;
    a.config.orderbook_grpc_url = fake.url.clone();
    let statuses = [
        tonic::Status::permission_denied("request refused (code 4081)"),
        tonic::Status::unimplemented("no control service"),
        tonic::Status::internal("boom"),
        tonic::Status::invalid_argument("request refused (code 4082)"),
        tonic::Status::unavailable("down"),
    ];
    for status in statuses {
        fake.state().log_failure = Some(status.clone());
        let guard = attach_with(&a.config, DAEMON, &a.env(None), &RpcLogSource).await;
        assert!(guard.is_ok_and(|g| !g.is_linked()), "{status:?}");
    }
    // A log that verifies under this key is the one refusal.
    fake.state().log_failure = None;
    fake.hold(&a.chain.entries);
    let err = format!("{:#}", attach_with(&a.config, DAEMON, &a.env(None), &RpcLogSource).await.unwrap_err());
    assert!(err.contains("cloud-agent control restore"), "{err}");
    // With the marker, a refusal or an unreadable log stops startup.
    let linked = agent([0x8e; 32], 1, 1, "probe-marker").await;
    let mut config = linked.config.clone();
    config.orderbook_grpc_url = fake.url.clone();
    fake.state().log_failure = Some(tonic::Status::permission_denied("request refused (code 4081)"));
    let err = format!("{:#}", attach_with(&config, DAEMON, &linked.env(Some(linked.head(1))), &RpcLogSource).await.unwrap_err());
    assert!(err.contains("refused the control log query"), "{err}");
    fake.state().log_failure = Some(tonic::Status::unimplemented(""));
    let err = format!("{:#}", attach_with(&config, DAEMON, &linked.env(Some(linked.head(1))), &RpcLogSource).await.unwrap_err());
    assert!(err.contains("cannot be read"), "{err}");
}

#[tokio::test]
async fn an_unlinked_agent_without_marker_or_store_starts_as_today() {
    let a = agent([0x86; 32], 1, 0, "unlinked").await;
    for source in [Fake::Unreachable, remote(&[])] {
        let opts = AttachOptions { purpose: Purpose::Agent, force: true, no_reject: true };
        let guard = attach_with(&a.config, opts, &a.env(None), &source).await.unwrap();
        assert!(!guard.is_linked() && !guard.strict());
    }
    let sign = AttachOptions { purpose: Purpose::Sign, force: false, no_reject: false };
    assert!(attach_with(&a.config, sign, &a.env(None), &remote(&[])).await.is_ok());
}

#[tokio::test]
async fn a_linked_agent_compares_its_log_with_the_rpc() {
    let a = agent([0x87; 32], 3, 2, "compare").await;
    let env = a.env(Some(a.head(2)));
    for ok in [remote(&a.chain.entries[..2]), remote(&a.chain.entries[..1]), remote(&[])] {
        attach_with(&a.config, DAEMON, &env, &ok).await.unwrap();
    }
    let err = refusal(&a, DAEMON, &env, &remote(&a.chain.entries)).await;
    assert!(err.contains("ahead of the local copy"), "{err}");
    // Another entry at seq 2, signed by the same key, is a fork.
    let mut fork = TestChain::new([0x87; 32], a.store.party(), Network::Devnet).unwrap();
    fork.created_at = fork.created_at.checked_add_secs(3600).unwrap();
    let link = fork.link_pending(0x21).unwrap();
    fork.push(&link).unwrap();
    assert_ne!(fork.entries[0].hash(), a.chain.entries[0].hash(), "a different link entry");
    let err = refusal(&a, DAEMON, &env, &remote(&fork.entries)).await;
    assert!(err.contains("diverges") || err.contains("invalid"), "{err}");
}

#[tokio::test]
async fn a_linked_agent_refuses_force_no_reject_and_sign() {
    let a = agent([0x88; 32], 1, 1, "bypass").await;
    let env = a.env(Some(a.head(1)));
    let source = remote(&a.chain.entries);
    let cases = [
        (AttachOptions { purpose: Purpose::Agent, force: true, no_reject: false }, "--force"),
        (AttachOptions { purpose: Purpose::Agent, force: false, no_reject: true }, "--no-reject"),
        (AttachOptions { purpose: Purpose::Sign, force: false, no_reject: false }, "sign command"),
    ];
    for (opts, expected) in cases {
        let err = refusal(&a, opts, &env, &source).await;
        assert!(err.contains(expected), "{err}");
    }
    let other = AttachOptions { purpose: Purpose::Other, force: false, no_reject: true };
    assert!(attach_with(&a.config, other, &env, &source).await.unwrap().is_linked());
    let control = AttachOptions { purpose: Purpose::Control, force: false, no_reject: false };
    assert!(attach_with(&a.config, control, &env, &Fake::Refused).await.is_ok(), "control commands compare logs themselves");
}

#[tokio::test]
async fn an_accepted_entry_is_appended_and_marked() {
    let mut a = agent([0x8a; 32], 1, 1, "mark").await;
    let env = a.env(Some(a.head(1)));
    let guard = attach_with(&a.config, DAEMON, &env, &remote(&a.chain.entries)).await.unwrap();
    let signer = agent_logic::signer::resolve_with(&a.config.private_key, &env).unwrap();
    let pending = a.chain.add_pending(0x91).unwrap();
    let signed = signer.sign_control_entry(&pending).unwrap();
    {
        let lock = a.store.lock().await.unwrap();
        a.store.record_pending(&lock, &signed, None, 1).unwrap();
    }
    let env_file = a.base.join(".env");
    std::fs::write(&env_file, "PARTY_AGENT=x\n").unwrap();
    let accepted = HeadRef { seq: 2, hash: *signed.hash() };
    assert_eq!(accept_and_mark(&a.store, &accepted, &guard, &env_file).await.unwrap(), accepted);
    assert_eq!(guard.head(), Some(accepted));
    let written = crate::read_env_value(&env_file, MARKER_ENV).unwrap();
    assert_eq!(ControlMarker::parse(&written).unwrap(), ControlMarker { network: Network::Devnet, head: accepted });
    assert_eq!(crate::read_env_value(&env_file, "PARTY_AGENT").as_deref(), Some("x"));
    // The new marker now protects the new head.
    a.chain.push(&pending).unwrap();
    let env = ControlEnv { marker: Some(written), ..env };
    attach_with(&a.config, DAEMON, &env, &remote(&a.chain.entries)).await.unwrap();
}

/// The rows of `entries` with seq `edit` changed so it no longer verifies, or dropped.
fn tampered(entries: &[agent_control::SignedEntry], seq: u64, drop_it: bool) -> RemoteLog {
    let mut log = RemoteLog::default();
    for entry in entries {
        let (mut bytes, signature) = (entry.bytes().to_vec(), *entry.signature());
        if entry.seq() == seq {
            if drop_it {
                log.gap_at = Some(seq);
                continue;
            }
            bytes.push(b'x');
        }
        if log.gap_at.is_some() {
            log.beyond_gap.push((entry.seq(), bytes, signature));
        } else {
            log.entries.push((bytes, signature));
        }
    }
    log.stored_tip = entries.last().map_or(0, |e| e.seq());
    log
}

#[tokio::test]
async fn drill_a_broken_remote_with_the_marker_starts_and_points_to_sync() {
    let a = agent([0x90; 32], 3, 3, "broken-start").await;
    let env = a.env(Some(a.head(3)));
    for drop_it in [false, true] {
        let source = Fake::Log(tampered(&a.chain.entries, 2, drop_it));
        let guard = attach_with(&a.config, DAEMON, &env, &source).await.unwrap();
        assert!(guard.is_linked() && guard.strict(), "the local copy stays in force");
        let signer = agent_logic::signer::resolve_with(&a.config.private_key, &env).unwrap();
        let marker = env.marker().unwrap();
        let notice = compare_with_rpc(&a.config, &signer, &guard, Network::Devnet, marker.as_ref(), &source)
            .await
            .unwrap()
            .unwrap();
        assert!(notice.contains("broken at seq 2") && notice.contains("cloud-agent control sync"), "{notice}");
        assert!(!notice.contains("refused the control log query"), "a gap is not an rpc refusal: {notice}");
    }
    // A plain tail behind is a notice too; the same rows at equal length are none.
    let signer = agent_logic::signer::resolve_with(&a.config.private_key, &env).unwrap();
    let guard = Arc::clone(signer.guard());
    let marker = env.marker().unwrap();
    let behind = compare_with_rpc(&a.config, &signer, &guard, Network::Devnet, marker.as_ref(), &remote(&a.chain.entries[..2])).await.unwrap();
    assert!(behind.is_some_and(|n| n.contains("behind") && n.contains("control sync")));
    let equal = compare_with_rpc(&a.config, &signer, &guard, Network::Devnet, marker.as_ref(), &remote(&a.chain.entries)).await.unwrap();
    assert!(equal.is_none());
}

#[tokio::test]
async fn a_validly_signed_divergent_row_past_a_broken_one_stops_startup() {
    let a = agent([0x91; 32], 3, 3, "fork-past-broken").await;
    let env = a.env(Some(a.head(3)));
    let mut fork = TestChain::build([0x91; 32], a.store.party(), Network::Devnet, 2).unwrap();
    fork.created_at = fork.created_at.checked_add_secs(3600).unwrap();
    let third = fork.add_pending(0x99).unwrap();
    let third = fork.push(&third).unwrap();
    let mut log = tampered(&a.chain.entries, 2, false);
    log.entries[2] = (third.bytes().to_vec(), *third.signature());
    let err = refusal(&a, DAEMON, &env, &Fake::Log(log)).await;
    assert!(err.contains("diverges from the local one at seq 3"), "{err}");
    // Rows past the local head that no key signed refuse too.
    let mut log = tampered(&a.chain.entries, 0, false);
    log.stored_tip = 5;
    let err = refusal(&a, DAEMON, &env, &Fake::Log(log)).await;
    assert!(err.contains("up to seq 5") && err.contains("did not sign"), "{err}");
}

#[tokio::test]
async fn the_marker_is_only_raised() {
    let a = agent([0x92; 32], 3, 3, "raise-only").await;
    let env_file = a.base.join(".env");
    std::fs::write(&env_file, "PARTY_AGENT=x\n").unwrap();
    record_marker(&env_file, Network::Devnet, a.head(2)).unwrap();
    let read = || ControlMarker::parse(&crate::read_env_value(&env_file, MARKER_ENV).unwrap()).unwrap();
    assert_eq!(read().head, a.head(2));
    record_marker(&env_file, Network::Devnet, a.head(3)).unwrap();
    assert_eq!(read().head, a.head(3));
    // A lower seq (a slower writer) keeps the higher marker; the same head is a no-op.
    record_marker(&env_file, Network::Devnet, a.head(1)).unwrap();
    record_marker(&env_file, Network::Devnet, a.head(3)).unwrap();
    assert_eq!(read().head, a.head(3));
    // Another hash at the marker's seq, another network or a malformed marker: refused.
    let forged = HeadRef { seq: 3, hash: a.head(1).hash };
    assert!(record_marker(&env_file, Network::Devnet, forged).unwrap_err().to_string().contains("another entry at seq 3"));
    assert!(record_marker(&env_file, Network::Mainnet, a.head(3)).unwrap_err().to_string().contains("another network"));
    crate::upsert_env_value(&env_file, MARKER_ENV, "bogus").unwrap();
    assert!(record_marker(&env_file, Network::Devnet, a.head(3)).is_err());
    assert_eq!(crate::read_env_value(&env_file, "PARTY_AGENT").as_deref(), Some("x"));
}

#[test]
fn read_env_value_matches_keys_as_upsert_env_value_does() {
    let base = temp_base("read-env");
    let env_file = base.join(".env");
    let read = |text: &str| {
        std::fs::write(&env_file, text).unwrap();
        crate::read_env_value(&env_file, MARKER_ENV)
    };
    assert_eq!(read("export AGENT_CONTROL_HEAD=exported\n").as_deref(), Some("exported"));
    assert_eq!(read("export\tAGENT_CONTROL_HEAD = \"tabbed\"\n").as_deref(), Some("tabbed"));
    assert_eq!(read("  AGENT_CONTROL_HEAD = spaced \n").as_deref(), Some("spaced"));
    assert_eq!(read("A=1\n  export   AGENT_CONTROL_HEAD='indented'\n").as_deref(), Some("indented"));
    // Comments, longer keys and a glued `export` are other keys, as for upsert_env_value.
    let other = "# AGENT_CONTROL_HEAD=commented\nAGENT_CONTROL_HEADX=longer\nexportAGENT_CONTROL_HEAD=glued\n";
    assert_eq!(read(other), None);
    // The first setting wins, which is the line upsert_env_value replaces.
    assert_eq!(read("AGENT_CONTROL_HEAD=first\nexport AGENT_CONTROL_HEAD=second\n").as_deref(), Some("first"));
    for text in ["export AGENT_CONTROL_HEAD=old\n", "AGENT_CONTROL_HEAD = old\n", other] {
        std::fs::write(&env_file, text).unwrap();
        crate::upsert_env_value(&env_file, MARKER_ENV, "new").unwrap();
        assert_eq!(crate::read_env_value(&env_file, MARKER_ENV).as_deref(), Some("new"), "{text:?}");
    }
}

#[test]
fn upsert_env_value_rewrites_atomically_and_matches_keys_as_dotenv_does() {
    let base = temp_base("upsert");
    let env_file = base.join(".env");
    // A new file is created 0600.
    crate::upsert_env_value(&env_file, "A", "1").unwrap();
    assert_eq!(std::fs::read_to_string(&env_file).unwrap(), "A=1\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&env_file).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let text = "# AGENT_CONTROL_HEAD=commented\nPARTY_AGENT_PRIVATE_KEY=secret\n  AGENT_CONTROL_HEAD=old\nAGENT_CONTROL_HEADX=keep\nexport AGENT_CONTROL_HEAD=dup\nAGENT_CONTROL_HEAD = spaced\n";
    std::fs::write(&env_file, text).unwrap();
    crate::upsert_env_value(&env_file, MARKER_ENV, "new").unwrap();
    assert_eq!(
        std::fs::read_to_string(&env_file).unwrap(),
        "# AGENT_CONTROL_HEAD=commented\nPARTY_AGENT_PRIVATE_KEY=secret\nAGENT_CONTROL_HEAD=new\nAGENT_CONTROL_HEADX=keep\n"
    );
    assert_eq!(crate::read_env_value(&env_file, MARKER_ENV).as_deref(), Some("new"));
    // An exported key is replaced in place; no temp file is left behind.
    std::fs::write(&env_file, "export PARTY_AGENT=a\nB=2\n").unwrap();
    crate::upsert_env_value(&env_file, "PARTY_AGENT", "b").unwrap();
    assert_eq!(std::fs::read_to_string(&env_file).unwrap(), "PARTY_AGENT=b\nB=2\n");
    let leftovers: Vec<_> = std::fs::read_dir(&base).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".tmp")).collect();
    assert!(leftovers.is_empty());
    // A symlinked env file is written through its link.
    #[cfg(unix)]
    {
        let real = base.join("real.env");
        std::fs::write(&real, "A=1\n").unwrap();
        let link = base.join("link.env");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        crate::upsert_env_value(&link, "A", "2").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "A=2\n");
    }
}
