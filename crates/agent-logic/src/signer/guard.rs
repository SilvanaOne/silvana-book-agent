//! Which intents the agent key may sign.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use agent_control::{
    AgentSignable, ControlState, HeadClaim, HeadRef, Network, PartyId, Receipt, SCHEME_CONTROL,
    SCHEME_HEAD, SCHEME_RECEIPT, SignablePurpose, VerifiedLog, replay,
};
use anyhow::{Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::authorize::{Authorization, Refusal, Spend};
use super::{FeeConsentKind, RequestKind, SigningIntent};
use crate::control_store::{ControlMarker, ControlStore, StoredLog};

/// Length of the Canton onboarding multihash.
pub const ONBOARDING_MULTIHASH_LEN: usize = 34;

/// First bytes of an order's canonical JSON (sorted keys).
const ORDER_PREFIX: &[u8] = b"{\"market_id\":";

static NEXT_GUARD_ID: AtomicU64 = AtomicU64::new(1);

/// Applies the signing rules for one agent key.
#[derive(Debug)]
pub struct ControlGuard {
    id: u64,
    public_key: [u8; 32],
    mode: RwLock<Mode>,
}

#[derive(Clone, Debug)]
enum Mode {
    /// No control log: today's rules, no policy.
    Unlinked,
    /// A verified control log is in force.
    Linked(Arc<LinkedView>),
}

/// The verified control log a linked guard enforces, with its replayed state.
#[derive(Clone, Debug)]
pub struct LinkedView {
    store: ControlStore,
    log: VerifiedLog,
    state: ControlState,
}

impl LinkedView {
    pub fn store(&self) -> &ControlStore {
        &self.store
    }

    pub fn log(&self) -> &VerifiedLog {
        &self.log
    }

    pub fn state(&self) -> &ControlState {
        &self.state
    }

    /// The last accepted entry.
    pub fn head(&self) -> HeadRef {
        self.log.head()
    }
}

/// What a reload did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reload {
    Unchanged,
    Advanced { from: Option<HeadRef>, to: HeadRef },
}

/// The linked view of `stored`, checked against the key and the marker.
fn view_from(
    store: &ControlStore,
    public_key: [u8; 32],
    stored: StoredLog,
    marker: Option<&ControlMarker>,
) -> Result<LinkedView> {
    ensure!(
        *stored.log.agent_key() == public_key,
        "the control log in {} belongs to another key",
        store.dir().display()
    );
    let state = replay(&stored.log, &stored.documents).map_err(|e| {
        anyhow!(
            "the control log in {} does not replay at seq {}: {} (run `cloud-agent control sync`)",
            store.dir().display(),
            e.seq,
            e.code_name()
        )
    })?;
    if let Some(marker) = marker {
        marker.check(&stored.log)?;
    }
    Ok(LinkedView {
        store: store.clone(),
        log: stored.log,
        state,
    })
}

/// `next` must contain the head of `current`: the guard never moves back.
fn ensure_forward(current: &VerifiedLog, next: &VerifiedLog) -> Result<()> {
    let head = current.head();
    match next.entry(head.seq) {
        None => bail!(
            "the control log on disk (seq {}) is behind the one in force (seq {}): refusing to move back",
            next.head().seq,
            head.seq
        ),
        Some(entry) if *entry.hash() != head.hash => bail!(
            "the control log on disk diverges from the one in force at seq {}",
            head.seq
        ),
        Some(_) => Ok(()),
    }
}

impl ControlGuard {
    fn with_mode(public_key: [u8; 32], mode: Mode) -> Self {
        Self {
            id: NEXT_GUARD_ID.fetch_add(1, Ordering::Relaxed),
            public_key,
            mode: RwLock::new(mode),
        }
    }

    /// The guard of an agent without a control log.
    pub fn unlinked(public_key: [u8; 32]) -> Self {
        Self::with_mode(public_key, Mode::Unlinked)
    }

    /// A linked guard from the store's verified log; it must be at or past `marker`.
    /// Reads without the lock and repairs nothing.
    pub fn load(
        store: ControlStore,
        public_key: [u8; 32],
        marker: Option<&ControlMarker>,
    ) -> Result<Self> {
        let stored = store
            .snapshot()?
            .ok_or_else(|| anyhow!("no control log in {}", store.dir().display()))?;
        let view = view_from(&store, public_key, stored, marker)?;
        Ok(Self::with_mode(public_key, Mode::Linked(Arc::new(view))))
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public_key
    }

    pub fn is_linked(&self) -> bool {
        matches!(*crate::sync::read(&self.mode), Mode::Linked(_))
    }

    /// Whether every transaction hash must be computed locally.
    pub fn strict(&self) -> bool {
        self.is_linked()
    }

    /// The last accepted control head; `None` while unlinked.
    pub fn head(&self) -> Option<HeadRef> {
        match &*crate::sync::read(&self.mode) {
            Mode::Unlinked => None,
            Mode::Linked(view) => Some(view.head()),
        }
    }

    /// The linked view in force.
    pub fn view(&self) -> Option<Arc<LinkedView>> {
        match &*crate::sync::read(&self.mode) {
            Mode::Unlinked => None,
            Mode::Linked(view) => Some(Arc::clone(view)),
        }
    }

    pub fn party(&self) -> Option<PartyId> {
        self.view().map(|v| v.state.agent_party().clone())
    }

    pub fn network(&self) -> Option<Network> {
        self.view().map(|v| v.state.network())
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Re-reads the linked store under its lock and moves forward only.
    pub async fn reload(&self) -> Result<Reload> {
        match self.view() {
            None => Ok(Reload::Unchanged),
            Some(view) => self.advance(&view.store, None).await,
        }
    }

    /// [`Self::reload`] from `store`, which may also link an unlinked guard.
    pub async fn reload_from(
        &self,
        store: &ControlStore,
        marker: Option<&ControlMarker>,
    ) -> Result<Reload> {
        if let Some(view) = self.view() {
            ensure!(
                view.store == *store,
                "the guard is linked to {}, not {}",
                view.store.dir().display(),
                store.dir().display()
            );
        }
        self.advance(store, marker).await
    }

    async fn advance(&self, store: &ControlStore, marker: Option<&ControlMarker>) -> Result<Reload> {
        // The lock is held until the swap so reloads apply in order.
        let lock = store.lock().await?;
        let stored = store.load(&lock)?;
        let Some(stored) = stored else {
            if self.is_linked() {
                bail!(
                    "the control log in {} is missing; the agent stays linked: run \
                     `cloud-agent control restore`",
                    store.dir().display()
                );
            }
            return Ok(Reload::Unchanged);
        };
        let next = view_from(store, self.public_key, stored, marker)?;
        let mut mode = crate::sync::write(&self.mode);
        let from = match &*mode {
            Mode::Linked(current) => {
                ensure_forward(&current.log, &next.log)?;
                if current.head() == next.head() {
                    return Ok(Reload::Unchanged);
                }
                Some(current.head())
            }
            Mode::Unlinked => None,
        };
        let to = next.head();
        *mode = Mode::Linked(Arc::new(next));
        drop(mode);
        drop(lock);
        Ok(Reload::Advanced { from, to })
    }

    /// Consents to `spends`; no policy caps them.
    pub async fn authorize(&self, _spends: &[Spend<'_>]) -> Result<Authorization, Refusal> {
        Ok(Authorization::unrestricted(self.id))
    }

    /// Returns the spends of an authorization that was not used.
    pub async fn release(&self, _authorization: Authorization) {}

    /// The rules every intent must pass before the key is opened.
    pub(crate) fn check(&self, intent: &SigningIntent<'_>) -> Result<()> {
        match intent {
            SigningIntent::Jwt(input) => self.check_jwt(input),
            SigningIntent::Request(canonical) => {
                ensure!(
                    RequestKind::of(canonical).is_some(),
                    "signing refused: not a request the agent sends"
                );
                Ok(())
            }
            SigningIntent::FeeConsent {
                canonical,
                authorization,
            } => {
                self.check_authorization(authorization)?;
                ensure!(
                    FeeConsentKind::of(canonical).is_some(),
                    "signing refused: not a fee authorization"
                );
                Ok(())
            }
            SigningIntent::Transaction {
                hash,
                authorization,
            } => {
                self.check_authorization(authorization)?;
                ensure!(
                    hash.bytes().iter().any(|b| *b != 0),
                    "signing refused: a transaction hash must be 32 non-zero bytes"
                );
                ensure!(
                    !(hash.is_server_fallback() && self.strict()),
                    "signing refused: the transaction hash was not computed locally"
                );
                Ok(())
            }
            SigningIntent::Order(data) => {
                ensure!(
                    data.starts_with(ORDER_PREFIX) && data.ends_with(b"}"),
                    "signing refused: not an order"
                );
                Ok(())
            }
            SigningIntent::OnboardingMultihash(multihash) => {
                ensure!(
                    multihash.len() == ONBOARDING_MULTIHASH_LEN,
                    "signing refused: the onboarding multihash must be {ONBOARDING_MULTIHASH_LEN} bytes"
                );
                self.require_unlinked()
            }
            SigningIntent::RawCli(bytes) => {
                self.require_unlinked()?;
                check_raw_cli(bytes)
            }
            SigningIntent::ControlEntry(signable) => {
                let scheme = format!("scheme={SCHEME_CONTROL}\n");
                ensure!(
                    signable.purpose() == SignablePurpose::LogEntry
                        && signable.bytes().starts_with(scheme.as_bytes()),
                    "signing refused: not a control log entry"
                );
                Ok(())
            }
            SigningIntent::Receipt(signable) => self.check_receipt(signable),
            SigningIntent::HeadClaim(signable) => self.check_head_claim(signable),
        }
    }

    /// A receipt of this linked agent, for its own party, network and key.
    fn check_receipt(&self, signable: &AgentSignable) -> Result<()> {
        ensure!(
            signable.purpose() == SignablePurpose::Receipt
                && signable.bytes().starts_with(format!("scheme={SCHEME_RECEIPT}\n").as_bytes()),
            "signing refused: not a receipt"
        );
        let receipt =
            Receipt::decode(signable.bytes()).map_err(|_| anyhow!("signing refused: not a receipt"))?;
        let view = self
            .view()
            .ok_or_else(|| anyhow!("signing refused: only a linked agent signs receipts"))?;
        ensure!(
            receipt.agent_key == self.public_key
                && receipt.agent_party == *view.state.agent_party()
                && receipt.network == view.state.network(),
            "signing refused: the receipt is for another agent"
        );
        ensure!(
            receipt.log_seq <= view.head().seq,
            "signing refused: the receipt names a seq beyond the accepted head"
        );
        Ok(())
    }

    /// A claim of exactly the head in force (an empty log while unlinked), for this key.
    fn check_head_claim(&self, signable: &AgentSignable) -> Result<()> {
        ensure!(
            signable.purpose() == SignablePurpose::HeadClaim
                && signable.bytes().starts_with(format!("scheme={SCHEME_HEAD}\n").as_bytes()),
            "signing refused: not a head claim"
        );
        let claim =
            HeadClaim::decode(signable.bytes()).map_err(|_| anyhow!("signing refused: not a head claim"))?;
        ensure!(
            claim.agent_key == self.public_key,
            "signing refused: the head claim is for another key"
        );
        match self.view() {
            None => ensure!(
                claim.head.is_none(),
                "signing refused: an unlinked agent claims only an empty log"
            ),
            Some(view) => ensure!(
                claim.agent_party == *view.state.agent_party()
                    && claim.network == view.state.network()
                    && claim.head == Some(view.head()),
                "signing refused: the head claim is not the accepted head"
            ),
        }
        Ok(())
    }

    fn check_authorization(&self, authorization: &Authorization) -> Result<()> {
        ensure!(
            authorization.issued_by(self.id),
            "signing refused: the authorization is for another key"
        );
        Ok(())
    }

    fn require_unlinked(&self) -> Result<()> {
        ensure!(
            !self.is_linked(),
            "signing refused: not allowed for a linked agent"
        );
        Ok(())
    }

    /// A JWT signing input: `header.claims`, EdDSA, carrying this key; linked tokens name
    /// the agent's party and carry the control head.
    fn check_jwt(&self, input: &[u8]) -> Result<()> {
        let mut parts = input.split(|b| *b == b'.');
        let (Some(header), Some(claims), None) = (parts.next(), parts.next(), parts.next()) else {
            bail!("signing refused: not a JWT");
        };
        let header = decode_segment(header)?;
        let x = URL_SAFE_NO_PAD.encode(self.public_key);
        ensure!(
            header.get("alg").and_then(|v| v.as_str()) == Some("EdDSA")
                && header.get("typ").and_then(|v| v.as_str()) == Some("JWT")
                && header.pointer("/jwk/x").and_then(|v| v.as_str()) == Some(x.as_str()),
            "signing refused: not a JWT for this key"
        );
        let claims = decode_segment(claims)?;
        ensure!(claims.is_object(), "signing refused: not a JWT");
        let seq = claims.get("ctl_seq");
        let head = claims.get("ctl_head");
        match self.view() {
            None => ensure!(
                seq.is_none() && head.is_none(),
                "signing refused: control head claims for an unlinked agent"
            ),
            Some(view) => {
                ensure!(
                    claims.get("sub").and_then(|v| v.as_str())
                        == Some(view.state.agent_party().as_str()),
                    "signing refused: a linked agent signs tokens only for its own party"
                );
                let expected = view.head();
                ensure!(
                    seq.and_then(|v| v.as_u64()) == Some(expected.seq)
                        && head.and_then(|v| v.as_str()) == Some(expected.hash.to_string().as_str()),
                    "signing refused: the token does not carry the control head"
                );
            }
        }
        Ok(())
    }
}

/// Bytes for the `sign` command: nothing the agent signs through another intent.
fn check_raw_cli(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() != 32,
        "signing refused: 32 bytes may be a transaction hash"
    );
    ensure!(
        !bytes.starts_with(b"scheme=silvana.agent."),
        "signing refused: an agent control document"
    );
    ensure!(
        !bytes.starts_with(b"msg_type="),
        "signing refused: an agent request or authorization"
    );
    ensure!(
        !(bytes.starts_with(b"eyJ") && bytes.contains(&b'.')),
        "signing refused: a JWT signing input"
    );
    Ok(())
}

fn decode_segment(segment: &[u8]) -> Result<serde_json::Value> {
    let raw = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| anyhow!("signing refused: not a JWT"))?;
    serde_json::from_slice(&raw).map_err(|_| anyhow!("signing refused: not a JWT"))
}
