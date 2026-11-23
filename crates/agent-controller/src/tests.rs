//! End-to-end tests against the fake rpc.

use std::time::Duration;

use agent_control::policy::PolicyInstrument;
use agent_control::{
    Amount, Capability, CommandKind, CommandOp, CommandScope, InstrumentId, InstrumentRef, Network,
    PartyId, ReceiptOutcome, ScopePair, Uuid7,
};
use orderbook_proto::orderbook::{ControlCommandStatus, ControlHead};

use crate::testkit::{ChallengeEdit, FakeRpc, TestAgent, controller_id, controller_key, key_file};
use crate::{ControllerError, PinCheck, PolicyBase, ReceiptCheck, Standing};

const AGENT: [u8; 32] = [0x51; 32];
const KEY: u8 = 0x61;

async fn linked() -> (FakeRpc, TestAgent) {
    let rpc = FakeRpc::start(Network::Devnet).await.unwrap();
    let agent = TestAgent::devnet(AGENT, &[KEY]).unwrap();
    rpc.publish(&agent);
    (rpc, agent)
}

fn amount(text: &str) -> Amount {
    Amount::from_decimal(rust_decimal::Decimal::from_str_exact(text).unwrap()).unwrap()
}

fn transfer(to: &PartyId, value: &str) -> CommandOp {
    CommandOp::TransferCc {
        receiver: to.clone(),
        amount: amount(value),
        description: None,
        settlement_proposal_id: None,
    }
}

#[tokio::test]
async fn signs_in_verifies_the_log_and_pins_its_head() {
    let (rpc, agent) = linked().await;
    let (client, key) = rpc.client(&agent, KEY).unwrap();
    let signed_in = client.sign_in().await.unwrap();
    assert!(signed_in.expires_at.is_some());
    let verified = client.agent().await.unwrap();
    assert_eq!(verified.head(), agent.head().unwrap());
    assert!(
        verified.disagreements().is_empty(),
        "{:?}",
        verified.disagreements()
    );
    assert_eq!(verified.pin(), PinCheck::New);
    assert!(matches!(
        verified.standing(),
        Standing::InEffect { pending: None, .. }
    ));
    assert!(verified.standing().allows(Capability::Direct));
    let pinned = client.pins().get(Network::Devnet, &agent.party).unwrap();
    assert_eq!(pinned, agent.head());
    assert_eq!(
        client.pins().path(),
        key.parent().unwrap().join(crate::SEEN_HEADS_FILE)
    );
    assert_eq!(rpc.state().sign_ins, 1);
    // The reads, as the rpc reports them.
    client.holdings().await.unwrap();
    client.traffic().await.unwrap();
    client.activity(10, 0).await.unwrap();
    client.ledger(10, 0).await.unwrap();
    client.packages(false).await.unwrap();
    client.end().await.unwrap();
    assert_eq!(
        rpc.state().session_count(),
        0,
        "ending the session drops it on the rpc"
    );
    assert!(!client.has_session());
}

#[tokio::test]
async fn the_log_is_paged_and_every_page_verified() {
    let rpc = FakeRpc::start(Network::Devnet).await.unwrap();
    let mut agent = TestAgent::devnet(AGENT, &[KEY]).unwrap();
    for seed in 0x70..0x75 {
        agent.add_ed25519(seed, "read", 0).unwrap();
    }
    rpc.publish(&agent);
    rpc.state().page_limit = 2;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    let verified = client.agent().await.unwrap();
    assert_eq!(verified.log().entries().len(), 6);
    assert_eq!(rpc.state().count("GetControllerLog"), 3);
}

#[tokio::test]
async fn a_policy_is_drafted_from_the_replay_submitted_and_accepted() {
    let (rpc, mut agent) = linked().await;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    let verified = client.agent().await.unwrap();
    let now = client
        .checked_now()
        .unwrap_or_else(|_| verified.checked_at());
    let mut rules = verified.base_rules(PolicyBase::InForce, now).unwrap();
    rules.paused = true;
    let draft = client.draft_policy(&verified, rules, 3600).unwrap();
    assert_eq!(draft.document.version, 1);
    assert_eq!(
        draft.document.prev_policy_hash,
        agent_control::Hash32([0; 32])
    );
    assert!(!draft.widens(), "a pause narrows");
    assert_eq!(draft.confirmation(), "sign policy v1");
    assert!(
        verified.base_rules(PolicyBase::Scheduled, now).is_err(),
        "nothing is scheduled"
    );
    let submitted = client.submit_policy(&verified, &draft).await.unwrap();
    assert_eq!(submitted.hash, draft.hash);
    assert_eq!(rpc.state().policies.len(), 1);

    // The agent accepts it; the next draft chains on it.
    let view = submitted.view.0.clone();
    let envelope = agent_control::ProofEnvelope::decode(&view.proof).unwrap();
    agent.accept(&view.document, &envelope).unwrap();
    rpc.publish(&agent);
    let verified = client.agent().await.unwrap();
    assert!(
        verified.disagreements().is_empty(),
        "{:?}",
        verified.disagreements()
    );
    let now = verified.checked_at();
    let rules = verified.base_rules(PolicyBase::InForce, now).unwrap();
    assert!(rules.paused);
    let mut widened = rules.clone();
    widened.paused = false;
    let next = client.draft_policy(&verified, widened, 600).unwrap();
    assert_eq!(next.document.version, 2);
    assert_eq!(next.document.prev_policy_hash, submitted.hash);
    assert!(next.widens(), "a resume widens");
    // An expired validity is refused before anything is signed.
    assert!(client.draft_policy(&verified, rules.clone(), 0).is_err());
    assert!(client.draft_policy(&verified, rules, 86_401).is_err());
}

#[tokio::test]
async fn a_command_is_submitted_and_its_receipt_checked() {
    let (rpc, agent) = linked().await;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    let verified = client.agent().await.unwrap();
    let receiver = crate::testkit::party_for([0x52; 32], "bob").unwrap();
    let draft = client
        .draft_command(&verified, transfer(&receiver, "1.50"), None, 600)
        .unwrap();
    assert_eq!(draft.confirmation(), "transfer 1.5 CC");
    assert_eq!(draft.command.log_seq, agent.head().unwrap().seq);
    assert_eq!(draft.command.log_head, agent.head().unwrap().hash);
    assert_eq!(
        draft.command.issued_at.secs_until(draft.command.expires_at),
        600
    );
    assert!(draft.code().starts_with("CMD-"));
    assert!(
        client
            .draft_command(&verified, transfer(&receiver, "1"), None, 901)
            .is_err()
    );
    let submitted = client.submit_command(&verified, &draft).await.unwrap();
    let id = submitted.command.command_id.clone();

    // Still pending: the wait ends unfinished.
    let waited = client
        .wait(&verified, &id, Duration::from_millis(10))
        .await
        .unwrap();
    assert!(!waited.finished);
    assert_eq!(waited.receipt, ReceiptCheck::None);

    // A forged receipt is shown as unverified.
    let forger = controller_key(0x7e);
    let forged = agent
        .receipt(
            &id,
            &submitted.bytes,
            ReceiptOutcome::Executed,
            Some(&forger),
        )
        .unwrap();
    rpc.state()
        .settle(id.as_str(), ControlCommandStatus::Executed, Some(forged));
    let waited = client
        .wait(&verified, &id, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(waited.finished);
    assert!(
        matches!(waited.receipt, ReceiptCheck::Unverified(_)),
        "{:?}",
        waited.receipt
    );

    // A receipt for other bytes is unverified too; the agent's own one verifies.
    let other = agent
        .receipt(&id, b"other", ReceiptOutcome::Executed, None)
        .unwrap();
    rpc.state()
        .settle(id.as_str(), ControlCommandStatus::Executed, Some(other));
    let waited = client.command(&verified, &id).await.unwrap();
    assert!(
        matches!(waited.receipt, ReceiptCheck::Unverified(ref why) if why.contains("other command bytes"))
    );
    let genuine = agent
        .receipt(&id, &submitted.bytes, ReceiptOutcome::Executed, None)
        .unwrap();
    rpc.state()
        .settle(id.as_str(), ControlCommandStatus::Executed, Some(genuine));
    let waited = client.command(&verified, &id).await.unwrap();
    assert!(waited.receipt.is_verified(), "{:?}", waited.receipt);
    let listed = client.commands(20, 0).await.unwrap();
    assert_eq!(listed.0.commands.len(), 1);
}

#[tokio::test]
async fn an_expiry_by_the_rpcs_clock_alone_leaves_the_wait_open() {
    let (rpc, agent) = linked().await;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    let verified = client.agent().await.unwrap();
    let receiver = crate::testkit::party_for([0x54; 32], "bob").unwrap();
    let draft = client
        .draft_command(&verified, transfer(&receiver, "1"), None, 600)
        .unwrap();
    let submitted = client.submit_command(&verified, &draft).await.unwrap();
    let id = submitted.command.command_id.clone();
    // Expired with no receipt: the agent may still settle it.
    rpc.state()
        .settle(id.as_str(), ControlCommandStatus::Expired, None);
    let waited = client
        .wait(&verified, &id, Duration::from_millis(10))
        .await
        .unwrap();
    assert!(!waited.finished);
    assert_eq!(waited.receipt, ReceiptCheck::None);
    // The agent's receipt, posted after the expiry, ends the wait.
    let genuine = agent
        .receipt(&id, &submitted.bytes, ReceiptOutcome::Executed, None)
        .unwrap();
    rpc.state()
        .settle(id.as_str(), ControlCommandStatus::Executed, Some(genuine));
    let waited = client
        .wait(&verified, &id, Duration::from_millis(10))
        .await
        .unwrap();
    assert!(waited.finished, "{:?}", waited.receipt);
    assert!(waited.receipt.is_verified(), "{:?}", waited.receipt);
}

#[tokio::test]
async fn a_lost_submit_answer_is_unconfirmed_and_a_receipt_counts_only_for_the_signed_bytes() {
    let (rpc, agent) = linked().await;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    let verified = client.agent().await.unwrap();
    let receiver = crate::testkit::party_for([0x53; 32], "bob").unwrap();
    rpc.state().lose_submit_answers = true;
    let draft = client
        .draft_command(&verified, transfer(&receiver, "1"), None, 600)
        .unwrap();
    let err = client.submit_command(&verified, &draft).await.unwrap_err();
    assert!(err.is_unconfirmed(), "{err}");
    assert_eq!(rpc.state().commands.len(), 1, "the rpc stored it");
    rpc.state().lose_submit_answers = false;
    let draft = client
        .draft_command(&verified, transfer(&receiver, "1"), None, 600)
        .unwrap();
    let submitted = client.submit_command(&verified, &draft).await.unwrap();
    let id = submitted.command.command_id.clone();
    // Other bytes under the same id, with the agent's genuine receipt for them.
    let mut other = submitted.command.clone();
    other.op = transfer(&receiver, "2");
    let other_bytes = other.encode().unwrap();
    let receipt = agent
        .receipt(&id, &other_bytes, ReceiptOutcome::Executed, None)
        .unwrap();
    for view in rpc
        .state()
        .commands
        .iter_mut()
        .filter(|v| v.command_id == id.as_str())
    {
        view.command = other_bytes.clone();
    }
    rpc.state()
        .settle(id.as_str(), ControlCommandStatus::Executed, Some(receipt));
    let waited = client.command(&verified, &id).await.unwrap();
    assert!(waited.receipt.is_verified(), "{:?}", waited.receipt);
    let bound = waited.bound_to(&submitted.bytes);
    assert!(
        matches!(bound.receipt, ReceiptCheck::Unverified(ref why) if why.contains("this key signed")),
        "{:?}",
        bound.receipt
    );
    let same = client.command(&verified, &id).await.unwrap();
    assert!(same.bound_to(&other_bytes).receipt.is_verified());
}

#[tokio::test]
async fn a_command_outside_the_scope_is_refused_before_it_is_sent() {
    let (rpc, mut agent) = linked().await;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    let verified = client.agent().await.unwrap();
    let now = verified.checked_at();
    let mut rules = verified.base_rules(PolicyBase::InForce, now).unwrap();
    rules.scopes = vec![CommandScope {
        controller: controller_id(KEY),
        commands: vec![CommandKind::Swap],
        pairs: Vec::new(),
        lps: Vec::new(),
    }];
    let draft = client.draft_policy(&verified, rules, 3600).unwrap();
    assert!(!draft.widens(), "adding a scope narrows");
    let submitted = client.submit_policy(&verified, &draft).await.unwrap();
    let envelope = agent_control::ProofEnvelope::decode(&submitted.view.0.proof).unwrap();
    agent.accept(&submitted.view.0.document, &envelope).unwrap();
    rpc.publish(&agent);
    let verified = client.agent().await.unwrap();
    let receiver = crate::testkit::party_for([0x53; 32], "bob").unwrap();
    let draft = client
        .draft_command(&verified, transfer(&receiver, "2"), None, 600)
        .unwrap();
    let err = client
        .submit_command(&verified, &draft)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("command_out_of_scope") && err.contains("1108"),
        "{err}"
    );
    assert_eq!(
        rpc.state().count("SubmitControllerCommand"),
        0,
        "nothing was sent"
    );
}

#[tokio::test]
async fn a_pair_scoped_swap_is_checked_locally_against_its_bounds() {
    let (rpc, mut agent) = linked().await;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    let verified = client.agent().await.unwrap();
    let admin = crate::testkit::party_for([0x56; 32], "registry").unwrap();
    let dso = crate::testkit::party_for([0x57; 32], "DSO").unwrap();
    rpc.state().cc = Some(agent_control::canton_coin(&dso).unwrap());
    let token = |id: &str| InstrumentRef {
        admin: admin.clone(),
        id: InstrumentId::parse(id).unwrap(),
    };
    let mut rules = verified
        .base_rules(PolicyBase::InForce, verified.checked_at())
        .unwrap();
    rules.scopes = vec![CommandScope {
        controller: controller_id(KEY),
        commands: vec![CommandKind::Swap],
        pairs: vec![ScopePair {
            sell: PolicyInstrument::Token(token("AAA")),
            buy: PolicyInstrument::Token(token("BBB")),
            min_price: amount("2"),
            max_sell: amount("100"),
        }],
        lps: Vec::new(),
    }];
    let draft = client.draft_policy(&verified, rules, 3600).unwrap();
    let submitted = client.submit_policy(&verified, &draft).await.unwrap();
    let envelope = agent_control::ProofEnvelope::decode(&submitted.view.0.proof).unwrap();
    agent.accept(&submitted.view.0.document, &envelope).unwrap();
    rpc.publish(&agent);
    let verified = client.agent().await.unwrap();
    let swap = |sold: &str, least: &str| CommandOp::Swap {
        sell: token("AAA"),
        sell_amount: amount(sold),
        buy: token("BBB"),
        min_receive: amount(least),
        max_fee_cc: Amount::ZERO,
    };
    let draft = client
        .draft_command(&verified, swap("10", "20"), None, 600)
        .unwrap();
    assert_eq!(draft.confirmation(), "swap 10 AAA for 20 BBB");
    client.submit_command(&verified, &draft).await.unwrap();
    for (sold, least, code) in [("10", "19", "1110"), ("101", "300", "1111")] {
        let draft = client
            .draft_command(&verified, swap(sold, least), None, 600)
            .unwrap();
        let err = client
            .submit_command(&verified, &draft)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(code), "{sold} for {least}: {err}");
    }
    let reversed = CommandOp::Swap {
        sell: token("BBB"),
        sell_amount: amount("1"),
        buy: token("AAA"),
        min_receive: amount("1"),
        max_fee_cc: Amount::ZERO,
    };
    let draft = client
        .draft_command(&verified, reversed, None, 600)
        .unwrap();
    assert!(
        client
            .submit_command(&verified, &draft)
            .await
            .unwrap_err()
            .to_string()
            .contains("1109")
    );
    assert_eq!(
        rpc.state().count("SubmitControllerCommand"),
        1,
        "only the swap within bounds was sent"
    );
}

#[tokio::test]
async fn after_a_4096_it_signs_in_again_exactly_once() {
    let (rpc, agent) = linked().await;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    client.agent().await.unwrap();
    assert_eq!(rpc.state().sign_ins, 1);
    rpc.state().expire_sessions();
    client.holdings().await.unwrap();
    assert_eq!(rpc.state().sign_ins, 2, "one re-sign-in after the 4096");
    rpc.state().always_no_session = true;
    let err = client.holdings().await.unwrap_err();
    assert_eq!(err.code(), Some(4096));
    assert_eq!(
        rpc.state().sign_ins,
        3,
        "exactly one more sign-in, then the 4096 stands"
    );
    assert_eq!(rpc.state().count("GetControllerHoldings"), 4);
}

#[tokio::test]
async fn a_refused_sign_in_asks_for_one_fresh_challenge() {
    let (rpc, agent) = linked().await;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    rpc.state().refuse_sign_ins = 1;
    client.sign_in().await.unwrap();
    assert_eq!(rpc.state().count("GetControllerChallenge"), 2);
    rpc.state().refuse_sign_ins = 5;
    let err = client.sign_in().await.unwrap_err();
    assert_eq!(err.code(), Some(4097));
    assert_eq!(
        rpc.state().count("GetControllerChallenge"),
        4,
        "one retry, not more"
    );
}

#[tokio::test]
async fn challenges_failing_the_checks_are_never_signed() {
    let (rpc, agent) = linked().await;
    let other_party = crate::testkit::party_for([0x54; 32], "agent-2").unwrap();
    let cases = [
        (
            ChallengeEdit::Rpc("orderbook-mainnet.silvana.dev".into()),
            "wrong_rpc",
        ),
        (ChallengeEdit::Party(other_party), "wrong_party"),
        (ChallengeEdit::Network(Network::Mainnet), "wrong_network"),
        (
            ChallengeEdit::Controller(controller_id(0x62)),
            "wrong_controller",
        ),
        (ChallengeEdit::IssuedAgo(121), "outside_window"),
        (
            ChallengeEdit::Garbage(b"scheme=silvana.agent.command.v1\n".to_vec()),
            "",
        ),
    ];
    for (edit, name) in cases {
        let (client, _) = rpc.client(&agent, KEY).unwrap();
        rpc.state().challenge_edit = Some(edit.clone());
        let before = rpc.state().count("ControllerSignIn");
        let err = client.sign_in().await.unwrap_err();
        assert!(
            matches!(err, ControllerError::Challenge(_)),
            "{edit:?}: {err}"
        );
        assert!(err.to_string().contains(name), "{edit:?}: {err}");
        assert_eq!(
            rpc.state().count("ControllerSignIn"),
            before,
            "{edit:?}: nothing was signed"
        );
    }
    rpc.state().challenge_edit = Some(ChallengeEdit::IssuedAgo(100));
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    client.sign_in().await.unwrap();
}

#[tokio::test]
async fn nothing_is_signed_while_the_clocks_differ_by_more_than_a_minute() {
    let (rpc, agent) = linked().await;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    rpc.state().clock_offset_secs = 90;
    let err = client.sign_in().await.unwrap_err();
    assert!(matches!(err, ControllerError::Clock(_)), "{err}");
    assert_eq!(rpc.state().count("ControllerSignIn"), 0);
    rpc.state().clock_offset_secs = 30;
    client.sign_in().await.unwrap();
}

#[tokio::test]
async fn a_pending_or_removed_controller_cannot_sign_in() {
    let rpc = FakeRpc::start(Network::Devnet).await.unwrap();
    let mut agent = TestAgent::devnet(AGENT, &[KEY]).unwrap();
    agent.add_ed25519(0x63, "direct", 1800).unwrap();
    rpc.publish(&agent);
    let (pending, _) = rpc.client(&agent, 0x63).unwrap();
    assert_eq!(pending.sign_in().await.unwrap_err().code(), Some(4097));
    // The owner key sees the pending add in the verified log.
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    let verified = client.agent().await.unwrap();
    let state = verified.state();
    let record = state.controller(&controller_id(0x63)).unwrap();
    assert!(record.pending().is_some());
    assert!(matches!(
        crate::Standing::of(state, &controller_id(0x63), verified.checked_at()),
        Standing::Pending { .. }
    ));
    // A withdrawn controller cannot sign in either.
    let mut removed = TestAgent::devnet(AGENT, &[KEY, 0x64]).unwrap();
    removed.remove(0x64, 3700).unwrap();
    rpc.publish(&removed);
    let (gone, _) = rpc.client(&removed, 0x64).unwrap();
    assert_eq!(gone.sign_in().await.unwrap_err().code(), Some(4097));
}

#[tokio::test]
async fn a_shorter_or_divergent_log_refuses_every_signature() {
    let rpc = FakeRpc::start(Network::Devnet).await.unwrap();
    let mut agent = TestAgent::devnet(AGENT, &[KEY]).unwrap();
    agent.add_ed25519(0x65, "read", 0).unwrap();
    rpc.publish(&agent);
    let key = key_file("pin", KEY).unwrap();
    let client = rpc.client_with_key(&agent, &key).unwrap();
    client.agent().await.unwrap();

    // The rpc withholds the last entry: shorter than seen before.
    rpc.state().rows.pop();
    let verified = client.agent().await.unwrap();
    assert!(matches!(verified.pin(), PinCheck::Shorter { .. }));
    let receiver = crate::testkit::party_for([0x55; 32], "bob").unwrap();
    let draft = client
        .draft_command(&verified, transfer(&receiver, "1"), None, 600)
        .unwrap();
    let err = client.submit_command(&verified, &draft).await.unwrap_err();
    assert!(matches!(err, ControllerError::Pin(_)), "{err}");
    let rules = verified
        .base_rules(PolicyBase::InForce, verified.checked_at())
        .unwrap();
    let policy = client.draft_policy(&verified, rules, 3600).unwrap();
    assert!(matches!(
        client.submit_policy(&verified, &policy).await.unwrap_err(),
        ControllerError::Pin(_)
    ));
    {
        let state = rpc.state();
        assert_eq!(
            state.count("SubmitControllerCommand") + state.count("SubmitControllerPolicy"),
            0
        );
    }

    // Another chain at the same seq: divergent.
    let mut fork = TestAgent::devnet(AGENT, &[KEY]).unwrap();
    fork.add_ed25519(0x66, "read", 0).unwrap();
    rpc.publish(&fork);
    let verified = client.agent().await.unwrap();
    assert!(matches!(verified.pin(), PinCheck::Divergent { .. }));
    assert!(verified.ensure_signing_allowed().is_err());
    // The pin is never lowered by what the rpc served.
    assert_eq!(
        client.pins().get(Network::Devnet, &agent.party).unwrap(),
        agent.head()
    );
}

#[tokio::test]
async fn withheld_or_edited_rows_and_missing_documents_do_not_verify() {
    let (rpc, mut agent) = linked().await;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    // An accepted policy needs its document.
    let verified = client.agent().await.unwrap();
    let mut rules = verified
        .base_rules(PolicyBase::InForce, verified.checked_at())
        .unwrap();
    rules.paused = true;
    let draft = client.draft_policy(&verified, rules, 3600).unwrap();
    let submitted = client.submit_policy(&verified, &draft).await.unwrap();
    let envelope = agent_control::ProofEnvelope::decode(&submitted.view.0.proof).unwrap();
    agent.accept(&submitted.view.0.document, &envelope).unwrap();
    rpc.publish(&agent);
    rpc.state().documents.clear();
    assert!(matches!(
        client.agent().await.unwrap_err(),
        ControllerError::Log(_)
    ));

    rpc.publish(&agent);
    let row = rpc.state().rows.first().cloned().unwrap();
    let mut edited = row.clone();
    if let Some(last) = edited.signature.last_mut() {
        *last ^= 1;
    }
    rpc.state().rows[0] = edited;
    assert!(matches!(
        client.agent().await.unwrap_err(),
        ControllerError::Log(_)
    ));

    rpc.publish(&agent);
    rpc.state().rows.remove(0);
    let err = client.agent().await.unwrap_err();
    assert!(err.to_string().contains("seq 2 where seq 1"), "{err}");
}

#[tokio::test]
async fn the_rpcs_disagreement_is_reported() {
    let (rpc, agent) = linked().await;
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    rpc.state().state_head = Some(ControlHead {
        seq: 9,
        hash: "00".repeat(32),
    });
    let verified = client.agent().await.unwrap();
    let text = verified.disagreements().join("; ");
    assert!(text.contains("the rpc reports head seq 9"), "{text}");
}

#[tokio::test]
async fn a_key_without_read_is_refused_the_reads_by_the_rpc() {
    let rpc = FakeRpc::start(Network::Devnet).await.unwrap();
    let agent = TestAgent::linked(
        AGENT,
        Network::Devnet,
        &[(KEY, "policy,read"), (0x67, "direct")],
    )
    .unwrap();
    rpc.publish(&agent);
    let (client, _) = rpc.client(&agent, 0x67).unwrap();
    let verified = client.agent().await.unwrap();
    assert!(
        verified.standing().allows(Capability::Direct)
            && !verified.standing().allows(Capability::Read)
    );
    assert_eq!(client.holdings().await.unwrap_err().code(), Some(4086));
    // A direct-only key signs no policy.
    let rules = verified
        .base_rules(PolicyBase::InForce, verified.checked_at())
        .unwrap();
    let draft = client.draft_policy(&verified, rules, 3600).unwrap();
    let err = client
        .submit_policy(&verified, &draft)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no policy capability"), "{err}");
}

#[tokio::test]
async fn a_mainnet_agent_works_on_mainnet_only() {
    let rpc = FakeRpc::start(Network::Mainnet).await.unwrap();
    let agent = TestAgent::linked(AGENT, Network::Mainnet, &[(KEY, "policy,direct,read")]).unwrap();
    rpc.publish(&agent);
    let (client, _) = rpc.client(&agent, KEY).unwrap();
    let verified = client.agent().await.unwrap();
    assert_eq!(verified.state().network(), Network::Mainnet);
    let _ = Uuid7::parse("00000000-0000-7000-8000-000000000000").unwrap();
}
