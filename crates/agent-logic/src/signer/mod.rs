//! The one place the agent key signs. Every use names a typed intent, and the
//! control guard's rules run before the key is opened.

mod authorize;
mod guard;
mod jwt;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use message_signing::MessageSignatureData;
use sha2::{Digest, Sha256};

use agent_control::{
    AgentSignable, AnyReceipt, HeadClaim, HeadRef, PartyId, PendingEntry, Receipt, SignedEntry,
    verify_party_key,
};

use crate::config::AtomicQuoteKey;
use crate::control_store::{
    CHAIN_ENV, ChainNetwork, ControlEnv, ControlLock, ControlStore, MARKER_ENV,
};
use crate::secret::Secret;

pub use authorize::{Authorization, Refusal, Spend};
pub use guard::{ControlGuard, LinkedView, ONBOARDING_MULTIHASH_LEN, Reload};
pub use jwt::{CONTROL_TOKEN_MAX_TTL_SECS, JwtRequest, control_audience, control_ttl_secs};

/// What one signature is for.
#[derive(Clone, Copy, Debug)]
pub enum SigningIntent<'a> {
    /// A JWT's `header.claims`.
    Jwt(&'a [u8]),
    /// A request canonical, `msg_type=<kind>\n...`.
    Request(&'a [u8]),
    /// A fee authorization canonical.
    FeeConsent {
        canonical: &'a [u8],
        authorization: &'a Authorization,
    },
    /// A prepared-transaction hash.
    Transaction {
        hash: &'a TxHash,
        authorization: &'a Authorization,
    },
    /// An order's canonical JSON.
    Order(&'a [u8]),
    /// The onboarding multihash.
    OnboardingMultihash(&'a [u8]),
    /// Bytes given to the `sign` command.
    RawCli(&'a [u8]),
    /// A control log entry, signed over its full bytes.
    ControlEntry(&'a AgentSignable),
    /// A receipt the control crate built, over its full bytes.
    Receipt(&'a AgentSignable),
    /// A head claim the control crate built, over its full bytes.
    HeadClaim(&'a AgentSignable),
}

impl SigningIntent<'_> {
    /// The bytes signed; requests and fee consents sign their SHA-256 (`ed25519-sha256-v1`).
    fn message(&self) -> Cow<'_, [u8]> {
        match self {
            Self::Request(canonical) | Self::FeeConsent { canonical, .. } => {
                Cow::Owned(Sha256::digest(canonical).to_vec())
            }
            Self::Transaction { hash, .. } => Cow::Borrowed(hash.bytes().as_slice()),
            Self::ControlEntry(signable) | Self::Receipt(signable) | Self::HeadClaim(signable) => {
                Cow::Borrowed(signable.bytes())
            }
            Self::Jwt(bytes)
            | Self::Order(bytes)
            | Self::OnboardingMultihash(bytes)
            | Self::RawCli(bytes) => Cow::Borrowed(bytes),
        }
    }
}

/// The requests the agent signs, by their `msg_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    PrepareTransaction,
    PrepareAtomicTransaction,
    ExecuteTransaction,
    PreparePayFee,
    ExecutePayFee,
    RegisterAgent,
    GetOnboardingStatus,
    SubmitOnboardingSignature,
}

impl RequestKind {
    pub const ALL: [Self; 8] = [
        Self::PrepareTransaction,
        Self::PrepareAtomicTransaction,
        Self::ExecuteTransaction,
        Self::PreparePayFee,
        Self::ExecutePayFee,
        Self::RegisterAgent,
        Self::GetOnboardingStatus,
        Self::SubmitOnboardingSignature,
    ];

    pub fn msg_type(self) -> &'static str {
        match self {
            Self::PrepareTransaction => "PrepareTransactionRequest",
            Self::PrepareAtomicTransaction => "PrepareAtomicTransactionRequest",
            Self::ExecuteTransaction => "ExecuteTransactionRequest",
            Self::PreparePayFee => "PreparePayFeeRequest",
            Self::ExecutePayFee => "ExecutePayFeeRequest",
            Self::RegisterAgent => "RegisterAgent",
            Self::GetOnboardingStatus => "GetOnboardingStatus",
            Self::SubmitOnboardingSignature => "SubmitOnboardingSignature",
        }
    }

    /// The kind named by the first line of `canonical`, if the agent sends it.
    pub fn of(canonical: &[u8]) -> Option<Self> {
        let kind = msg_type_of(canonical)?;
        Self::ALL
            .into_iter()
            .find(|k| k.msg_type().as_bytes() == kind)
    }
}

/// The fee authorizations the agent signs, by their `msg_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeeConsentKind {
    Transaction,
    PayFee,
}

impl FeeConsentKind {
    pub fn msg_type(self) -> &'static str {
        match self {
            Self::Transaction => "TxFeesAuthorization",
            Self::PayFee => "PayFeeAuthorization",
        }
    }

    /// The kind named by the first line of `canonical`, if the agent signs it.
    pub fn of(canonical: &[u8]) -> Option<Self> {
        let kind = msg_type_of(canonical)?;
        [Self::Transaction, Self::PayFee]
            .into_iter()
            .find(|k| k.msg_type().as_bytes() == kind)
    }
}

/// The value of a leading `msg_type=<kind>\n` line.
fn msg_type_of(canonical: &[u8]) -> Option<&[u8]> {
    let rest = canonical.strip_prefix(b"msg_type=")?;
    let end = rest.iter().position(|b| *b == b'\n')?;
    rest.get(..end)
}

/// A prepared-transaction hash to sign: exactly 32 bytes, never all zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxHash {
    /// Computed locally from the prepared transaction.
    SelfComputed([u8; 32]),
    /// The ledger-service's hash, used when none was computed locally.
    ServerFallback([u8; 32]),
}

impl TxHash {
    pub fn self_computed(hash: [u8; 32]) -> Result<Self> {
        non_zero(hash).map(Self::SelfComputed)
    }

    pub fn server_fallback(hash: &[u8]) -> Result<Self> {
        let hash: [u8; 32] = hash.try_into().map_err(|_| {
            anyhow!("signing refused: a transaction hash must be 32 non-zero bytes")
        })?;
        non_zero(hash).map(Self::ServerFallback)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        match self {
            Self::SelfComputed(hash) | Self::ServerFallback(hash) => hash,
        }
    }

    pub fn is_server_fallback(&self) -> bool {
        matches!(self, Self::ServerFallback(_))
    }
}

fn non_zero(hash: [u8; 32]) -> Result<[u8; 32]> {
    ensure!(
        hash.iter().any(|b| *b != 0),
        "signing refused: a transaction hash must be 32 non-zero bytes"
    );
    Ok(hash)
}

/// A fee schedule the agent consents to, bound to its context.
#[derive(Clone, Copy, Debug)]
pub enum FeeConsent<'a> {
    /// The fees of the prepared transaction `transaction_id`.
    Transaction {
        party_id: &'a str,
        transaction_id: &'a str,
        fees_json: &'a str,
    },
    /// The processing fee of `proposal_id` in the pay-fee session `session_id`.
    PayFee {
        party_id: &'a str,
        session_id: &'a str,
        proposal_id: &'a str,
        fee_type: &'a str,
        fees_json: &'a str,
    },
}

impl<'a> FeeConsent<'a> {
    pub fn canonical(&self) -> Vec<u8> {
        match *self {
            Self::Transaction {
                party_id,
                transaction_id,
                fees_json,
            } => message_signing::canonical_tx_fees_authorization(
                party_id,
                transaction_id,
                fees_json,
            ),
            Self::PayFee {
                party_id,
                session_id,
                proposal_id,
                fee_type,
                fees_json,
            } => message_signing::canonical_pay_fee_authorization(
                party_id,
                session_id,
                proposal_id,
                fee_type,
                fees_json,
            ),
        }
    }

    /// The spend this consent needs authorized.
    pub fn spend(&self) -> Spend<'a> {
        match *self {
            Self::Transaction {
                transaction_id,
                fees_json,
                ..
            } => Spend::FeeConsent {
                key: transaction_id,
                fees_json,
            },
            Self::PayFee {
                session_id,
                fees_json,
                ..
            } => Spend::FeeConsent {
                key: session_id,
                fees_json,
            },
        }
    }
}

/// An order the agent places.
#[derive(Clone, Copy, Debug)]
pub struct OrderToSign<'a> {
    pub market_id: &'a str,
    pub order_type: &'a str,
    pub price: &'a str,
    pub quantity: &'a str,
    pub nonce: u64,
}

/// A signed order: base64 signature over `signed_data`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedOrder {
    pub signature: String,
    pub signed_data: Vec<u8>,
}

/// What opening the key produced.
struct Opened {
    verifying_key: VerifyingKey,
    signature: Option<Signature>,
}

/// A control head the local store holds, as its pending entry or in its log.
#[derive(Clone, Debug)]
struct HeldHead {
    head: HeadRef,
    party: PartyId,
    network: agent_control::Network,
}

/// The rules that run before the key is opened.
#[derive(Clone, Copy)]
enum Rules<'a> {
    /// The control guard's rules for the intent.
    Guard(&'a ControlGuard),
    /// An append token: a short JWT of this key and party claiming exactly a held head.
    HeldHeadToken(&'a ControlGuard, &'a HeldHead),
}

impl Rules<'_> {
    fn check(&self, intent: &SigningIntent<'_>) -> Result<()> {
        match (self, intent) {
            (Self::Guard(guard), _) => guard.check(intent),
            (Self::HeldHeadToken(guard, held), SigningIntent::Jwt(input)) => jwt::check_head_token(
                input,
                &guard.public_key(),
                held.party.as_str(),
                &held.head,
                held.network,
            ),
            (Self::HeldHeadToken(..), _) => bail!("signing refused: a head token is a JWT"),
        }
    }
}

/// Opens the sealed key: the only place the secret is exposed. With an intent, its
/// rules run first and exactly that intent's bytes are signed.
fn use_key(key: &Secret<32>, sign: Option<(Rules<'_>, &SigningIntent<'_>)>) -> Result<Opened> {
    if let Some((rules, intent)) = sign {
        rules.check(intent)?;
    }
    let seed = key.expose()?;
    let signing_key = SigningKey::from_bytes(&seed);
    let signature = sign.map(|(_, intent)| signing_key.sign(&intent.message()));
    Ok(Opened {
        verifying_key: signing_key.verifying_key(),
        signature,
    })
}

/// Signs for one agent key under its control guard.
#[derive(Clone)]
pub struct AgentSigner {
    key: Secret<32>,
    verifying_key: VerifyingKey,
    guard: Arc<ControlGuard>,
}

impl fmt::Debug for AgentSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentSigner")
            .field("public_key", &hex::encode(self.verifying_key.as_bytes()))
            .finish_non_exhaustive()
    }
}

impl AgentSigner {
    /// The signer for a base58 private key (32-byte seed or 64-byte keypair).
    pub fn from_base58(private_key_b58: &str) -> Result<Self> {
        let mut seed = crate::config::decode_private_key(private_key_b58)?;
        let key = Secret::seal(&mut seed)?;
        resolve(&key)
    }

    /// The all-zero key of clients that send no authorization; never registered.
    pub fn anonymous() -> Result<Self> {
        let key = Secret::seal(&mut [0u8; 32])?;
        let verifying_key = use_key(&key, None)?.verifying_key;
        Ok(Self {
            key,
            verifying_key,
            guard: Arc::new(ControlGuard::unlinked(verifying_key.to_bytes())),
        })
    }

    /// A signer whose key never opens, for tests of the failure paths.
    #[doc(hidden)]
    pub fn corrupt_for_tests() -> Self {
        let verifying_key = VerifyingKey::default();
        Self {
            key: Secret::corrupt_for_tests(),
            verifying_key,
            guard: Arc::new(ControlGuard::unlinked(verifying_key.to_bytes())),
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.verifying_key.to_bytes()
    }

    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    pub fn guard(&self) -> &Arc<ControlGuard> {
        &self.guard
    }

    pub async fn authorize(&self, spends: &[Spend<'_>]) -> Result<Authorization, Refusal> {
        self.guard.authorize(spends).await
    }

    pub async fn release(&self, authorization: Authorization) {
        self.guard.release(authorization).await
    }

    /// A JWT for `req`, issued now; a linked agent's carries its control head.
    pub fn sign_jwt(&self, req: &JwtRequest<'_>) -> Result<String> {
        self.sign_jwt_at(req, jwt::now_secs()?)
    }

    /// [`Self::sign_jwt`] with the control head it carries.
    pub fn sign_jwt_with_head(&self, req: &JwtRequest<'_>) -> Result<(String, Option<HeadRef>)> {
        self.sign_jwt_with_head_at(req, jwt::now_secs()?, None)
    }

    /// A token for the control services of `network` (`aud` names them), valid at most
    /// [`CONTROL_TOKEN_MAX_TTL_SECS`].
    pub fn sign_control_jwt(
        &self,
        req: &JwtRequest<'_>,
        network: agent_control::Network,
    ) -> Result<(String, Option<HeadRef>)> {
        let req = JwtRequest {
            ttl_secs: control_ttl_secs(req.ttl_secs),
            ..*req
        };
        let audience = control_audience(network);
        self.sign_jwt_with_head_at(&req, jwt::now_secs()?, Some(&audience))
    }

    /// A control token for the store's network claiming `head`, the head after an append
    /// (at most [`CONTROL_TOKEN_MAX_TTL_SECS`]); `store` must hold it, pending or in its log.
    pub fn sign_control_jwt_for_head(
        &self,
        req: &JwtRequest<'_>,
        store: &ControlStore,
        lock: &ControlLock,
        head: &HeadRef,
    ) -> Result<String> {
        let held = self.held_head(store, lock, head)?;
        let req = JwtRequest {
            ttl_secs: control_ttl_secs(req.ttl_secs),
            ..*req
        };
        let audience = control_audience(held.network);
        let input = jwt::signing_input_for(
            &self.public_key(),
            &req,
            jwt::now_secs()?,
            Some(head),
            Some(&audience),
        )?;
        let signature = self.sign_under(
            Rules::HeldHeadToken(&self.guard, &held),
            &SigningIntent::Jwt(input.as_bytes()),
        )?;
        Ok(format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }

    /// `head` as this key's store holds it: the pending entry or an entry of the log.
    fn held_head(
        &self,
        store: &ControlStore,
        lock: &ControlLock,
        head: &HeadRef,
    ) -> Result<HeldHead> {
        ensure!(
            verify_party_key(store.party(), &self.public_key()),
            "signing refused: the control store is for another key"
        );
        if let Some(view) = self.guard.view() {
            ensure!(
                view.store() == store,
                "signing refused: the control store is not the one in force"
            );
        }
        let stored = store.load(lock)?;
        let pending = match &stored {
            Some(stored) => stored.pending.clone(),
            None => store.pending(lock)?,
        };
        let in_log = stored
            .as_ref()
            .and_then(|s| s.log.entry(head.seq))
            .is_some_and(|entry| *entry.hash() == head.hash);
        let is_pending = pending
            .as_ref()
            .is_some_and(|p| p.entry.seq() == head.seq && *p.entry.hash() == head.hash);
        ensure!(
            in_log || is_pending,
            "signing refused: the local control store does not hold head seq {}",
            head.seq
        );
        Ok(HeldHead {
            head: *head,
            party: store.party().clone(),
            network: store.network(),
        })
    }

    /// A JWT for `req`, issued at `now` (seconds since the epoch).
    pub(crate) fn sign_jwt_at(&self, req: &JwtRequest<'_>, now: u64) -> Result<String> {
        self.sign_jwt_with_head_at(req, now, None).map(|(token, _)| token)
    }

    fn sign_jwt_with_head_at(
        &self,
        req: &JwtRequest<'_>,
        now: u64,
        audience: Option<&str>,
    ) -> Result<(String, Option<HeadRef>)> {
        // A reload between building and checking changes the head; build once more.
        let mut last = None;
        for _ in 0..2 {
            let head = self.guard.head();
            let input =
                jwt::signing_input_for(&self.public_key(), req, now, head.as_ref(), audience)?;
            match self.sign(&SigningIntent::Jwt(input.as_bytes())) {
                Ok(signature) => {
                    let token = format!(
                        "{input}.{}",
                        URL_SAFE_NO_PAD.encode(signature.to_bytes())
                    );
                    return Ok((token, head));
                }
                Err(e) if self.guard.head() != head => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("signing refused: the control head kept changing")))
    }

    /// Signs a built control log entry for this key; it must follow the guard's head.
    pub fn sign_control_entry(&self, pending: &PendingEntry) -> Result<SignedEntry> {
        let entry = pending.entry();
        ensure!(
            *entry.agent_key() == self.public_key(),
            "signing refused: the control entry is for another key"
        );
        match self.guard.view() {
            None => ensure!(
                entry.seq() == 1 && entry.prev().as_bytes().iter().all(|b| *b == 0),
                "signing refused: an unlinked agent signs only the first control entry"
            ),
            Some(view) => {
                let head = view.head();
                ensure!(
                    entry.agent_party() == view.state().agent_party()
                        && entry.network() == view.state().network(),
                    "signing refused: the control entry is for another party or network"
                );
                ensure!(
                    Some(entry.seq()) == head.seq.checked_add(1) && *entry.prev() == head.hash,
                    "signing refused: the control entry (seq {}) does not follow the head (seq {})",
                    entry.seq(),
                    head.seq
                );
            }
        }
        let signature = self.sign(&SigningIntent::ControlEntry(pending.signable()))?;
        pending
            .finish(signature.to_bytes())
            .map_err(|e| anyhow!("signing failed: {}", e.code_name()))
    }

    /// Signs a receipt over its canonical bytes; only a linked agent signs receipts.
    pub fn sign_receipt(&self, receipt: &Receipt) -> Result<[u8; 64]> {
        let signable = receipt
            .signable()
            .map_err(|e| anyhow!("signing refused: the receipt does not encode ({})", e.code_name()))?;
        Ok(self.sign(&SigningIntent::Receipt(&signable))?.to_bytes())
    }

    /// [`Self::sign_receipt`] for either version; v2 encodes only for an executed command.
    pub fn sign_any_receipt(&self, receipt: &AnyReceipt) -> Result<[u8; 64]> {
        let signable = receipt
            .signable()
            .map_err(|e| anyhow!("signing refused: the receipt does not encode ({})", e.code_name()))?;
        Ok(self.sign(&SigningIntent::Receipt(&signable))?.to_bytes())
    }

    /// Signs a claim of the accepted head over its canonical bytes.
    pub fn sign_head_claim(&self, claim: &HeadClaim) -> Result<[u8; 64]> {
        let signable = claim
            .signable()
            .map_err(|e| anyhow!("signing refused: the head claim does not encode ({})", e.code_name()))?;
        Ok(self.sign(&SigningIntent::HeadClaim(&signable))?.to_bytes())
    }

    /// The `ed25519-sha256-v1` signature of a request canonical.
    pub fn sign_request(&self, canonical: &[u8]) -> Result<MessageSignatureData> {
        let signature = self.sign(&SigningIntent::Request(canonical))?;
        Ok(self.message_signature(&signature))
    }

    /// The `ed25519-sha256-v1` signature of a fee authorization.
    pub fn sign_fee_consent(
        &self,
        authorization: &Authorization,
        consent: &FeeConsent<'_>,
    ) -> Result<MessageSignatureData> {
        let canonical = consent.canonical();
        let signature = self.sign(&SigningIntent::FeeConsent {
            canonical: &canonical,
            authorization,
        })?;
        Ok(self.message_signature(&signature))
    }

    /// The base64 signature of a prepared-transaction hash.
    pub fn sign_transaction(&self, authorization: &Authorization, hash: &TxHash) -> Result<String> {
        let signature = self.sign(&SigningIntent::Transaction {
            hash,
            authorization,
        })?;
        Ok(STANDARD.encode(signature.to_bytes()))
    }

    /// Signs an order's canonical JSON (sorted keys, placed by the agent).
    pub fn sign_order(&self, order: &OrderToSign<'_>) -> Result<SignedOrder> {
        let mut fields = BTreeMap::new();
        fields.insert(
            "market_id",
            serde_json::Value::String(order.market_id.to_string()),
        );
        fields.insert("nonce", serde_json::json!(order.nonce));
        fields.insert(
            "order_type",
            serde_json::Value::String(order.order_type.to_string()),
        );
        fields.insert("placed_by", serde_json::Value::String("agent".to_string()));
        fields.insert("price", serde_json::Value::String(order.price.to_string()));
        fields.insert(
            "quantity",
            serde_json::Value::String(order.quantity.to_string()),
        );
        let signed_data =
            serde_json::to_vec(&fields).context("failed to serialize order for signing")?;
        let signature = self.sign(&SigningIntent::Order(&signed_data))?;
        Ok(SignedOrder {
            signature: STANDARD.encode(signature.to_bytes()),
            signed_data,
        })
    }

    /// The base64 signature of the onboarding multihash.
    pub fn sign_onboarding_multihash(&self, multihash: &[u8]) -> Result<String> {
        let signature = self.sign(&SigningIntent::OnboardingMultihash(multihash))?;
        Ok(STANDARD.encode(signature.to_bytes()))
    }

    /// The base64 signature of bytes given to the `sign` command; refused for a key with a
    /// control store under the default control directory.
    pub fn sign_raw_cli(&self, bytes: &[u8]) -> Result<String> {
        self.sign_raw_cli_with(bytes, &ControlEnv::from_process())
    }

    /// [`Self::sign_raw_cli`] with the control directory of `env`.
    pub fn sign_raw_cli_with(&self, bytes: &[u8], env: &ControlEnv) -> Result<String> {
        refuse_with_control_store(&self.public_key(), env)?;
        let signature = self.sign(&SigningIntent::RawCli(bytes))?;
        Ok(STANDARD.encode(signature.to_bytes()))
    }

    fn sign(&self, intent: &SigningIntent<'_>) -> Result<Signature> {
        self.sign_under(Rules::Guard(&self.guard), intent)
    }

    fn sign_under(&self, rules: Rules<'_>, intent: &SigningIntent<'_>) -> Result<Signature> {
        ensure!(
            self.guard.public_key() == self.public_key(),
            "signing refused: the guard is for another key"
        );
        let opened = use_key(&self.key, Some((rules, intent)))?;
        ensure!(
            opened.verifying_key == self.verifying_key,
            "signing refused: the key changed"
        );
        opened.signature.ok_or_else(|| anyhow!("signing failed"))
    }

    fn message_signature(&self, signature: &Signature) -> MessageSignatureData {
        MessageSignatureData {
            signature_b64: STANDARD.encode(signature.to_bytes()),
            public_key_b64url: URL_SAFE_NO_PAD.encode(self.verifying_key.as_bytes()),
            signing_scheme: message_signing::SIGNING_SCHEME.to_string(),
        }
    }
}

/// The quote a signature commits to.
#[derive(Clone, Copy, Debug)]
pub struct QuoteIntent<'a> {
    pub quote_id: &'a str,
    pub market_id: &'a str,
    pub side: &'a str,
    pub base_amount: &'a str,
    pub quote_amount: &'a str,
    pub user_party: &'a str,
    pub lp_fee: Option<&'a str>,
}

/// Signs RFQ V2 quotes with the quote key, under the agent's guard.
#[derive(Clone)]
pub struct QuoteSigner {
    key: AtomicQuoteKey,
    guard: Arc<ControlGuard>,
}

impl fmt::Debug for QuoteSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuoteSigner")
            .field("pub_spki_hex", &self.key.pub_spki_hex)
            .finish_non_exhaustive()
    }
}

impl QuoteSigner {
    pub fn new(key: AtomicQuoteKey, guard: Arc<ControlGuard>) -> Self {
        Self { key, guard }
    }

    pub fn pub_spki_hex(&self) -> &str {
        &self.key.pub_spki_hex
    }

    pub fn guard(&self) -> &Arc<ControlGuard> {
        &self.guard
    }

    /// Authorizes the quote, then signs `canonical` (DER hex); a failed signature releases it.
    pub async fn authorize_and_sign(
        &self,
        intent: &QuoteIntent<'_>,
        canonical: &str,
    ) -> Result<String> {
        let authorization = self
            .guard
            .authorize(&[Spend::Quote {
                quote_id: intent.quote_id,
            }])
            .await?;
        match self.use_quote_key(&authorization, intent, canonical) {
            Ok(signature) => Ok(signature),
            Err(e) => {
                self.guard.release(authorization).await;
                Err(e)
            }
        }
    }

    /// Opens the quote key: the only place its scalar is exposed.
    fn use_quote_key(
        &self,
        authorization: &Authorization,
        intent: &QuoteIntent<'_>,
        canonical: &str,
    ) -> Result<String> {
        ensure!(
            authorization.issued_by(self.guard.id()),
            "signing refused: the authorization is for another key"
        );
        ensure!(
            canonical.starts_with("msg_type=silvana.atomic-dvp.quote.")
                && canonical.contains(&format!("\nquote_nonce={}\n", intent.quote_id)),
            "signing refused: not the canonical of this quote"
        );
        let scalar = self.key.scalar()?;
        atomic_quote::sign_quote_scalar(&scalar, canonical)
    }
}

/// Most keys one process signs for.
const MAX_GUARDS: usize = 4096;

/// One guard per public key, shared by every signer for that key.
static GUARDS: Mutex<BTreeMap<[u8; 32], Arc<ControlGuard>>> = Mutex::new(BTreeMap::new());

/// Installs `guard` for its key; a different guard for the same key is refused.
pub fn install(guard: Arc<ControlGuard>) -> Result<()> {
    let mut guards = crate::sync::lock(&GUARDS);
    match guards.get(&guard.public_key()) {
        Some(existing) if Arc::ptr_eq(existing, &guard) => Ok(()),
        Some(_) => bail!("a different control guard is already installed for this key"),
        None => {
            ensure!(
                guards.len() < MAX_GUARDS,
                "too many signing keys in one process"
            );
            guards.insert(guard.public_key(), guard);
            Ok(())
        }
    }
}

/// The guard installed for `public_key`, if any.
pub fn installed(public_key: &[u8; 32]) -> Option<Arc<ControlGuard>> {
    crate::sync::lock(&GUARDS).get(public_key).map(Arc::clone)
}

/// Installs `guard` unless one is already installed for its key, which is returned instead.
fn install_or_existing(guard: Arc<ControlGuard>) -> Result<Arc<ControlGuard>> {
    let mut guards = crate::sync::lock(&GUARDS);
    if let Some(existing) = guards.get(&guard.public_key()) {
        return Ok(Arc::clone(existing));
    }
    ensure!(
        guards.len() < MAX_GUARDS,
        "too many signing keys in one process"
    );
    guards.insert(guard.public_key(), Arc::clone(&guard));
    Ok(guard)
}

/// The store of `public_key` on `network` in the agent folder, moved there from an earlier
/// version's location first: the `PARTY_AGENT` one when it is this key's.
fn find_store(
    network: agent_control::Network,
    public_key: &[u8; 32],
    env: &ControlEnv,
) -> Result<Option<ControlStore>> {
    let party = env
        .party
        .as_deref()
        .and_then(|p| PartyId::parse(p.trim()).ok())
        .filter(|p| verify_party_key(p, public_key));
    match party {
        Some(party) => {
            let store = ControlStore::locate(env, network, &party)?;
            Ok(store.exists().then_some(store))
        }
        None => Ok(ControlStore::locate_for_key(env, network, public_key)?.filter(ControlStore::exists)),
    }
}

/// Refuses when the agent folder of `env`, or an earlier version's location, holds a
/// store of `public_key` on any network: that agent is linked or linking.
fn refuse_with_control_store(public_key: &[u8; 32], env: &ControlEnv) -> Result<()> {
    let refuse = |dir: &std::path::Path| {
        anyhow!(
            "signing refused: this key has a control store in {}; the sign command is \
             refused for a linked agent",
            dir.display()
        )
    };
    if let Ok(base) = env.base() {
        let party = ControlStore::stored_party_in(&base)
            .context("signing refused: the control directory cannot be checked")?;
        if party.is_some_and(|party| verify_party_key(&party, public_key)) {
            return Err(refuse(&base.join(crate::control_store::CONTROL_DIR)));
        }
    }
    for network in [agent_control::Network::Devnet, agent_control::Network::Mainnet] {
        if let Some(legacy) = env.legacy_base() {
            let found = ControlStore::find_legacy(&legacy, network, public_key)
                .context("signing refused: the control directory cannot be checked")?;
            if let Some(store) = found.filter(ControlStore::exists) {
                return Err(refuse(store.dir()));
            }
        }
    }
    Ok(())
}

/// The guard for a key no guard is installed for: linked when its store holds a log.
/// With the linked marker set, a missing, unloadable or older store is refused.
pub fn discover(public_key: [u8; 32], env: &ControlEnv) -> Result<ControlGuard> {
    let marker = if env.marker_covers(&public_key) {
        env.marker()?
    } else {
        None
    };
    let Some(marker) = marker else {
        let ChainNetwork::Mapped(network) = env.network() else {
            return Ok(ControlGuard::unlinked(public_key));
        };
        if env.base().is_err() {
            return Ok(ControlGuard::unlinked(public_key));
        }
        return match find_store(network, &public_key, env)? {
            Some(store) if store.snapshot()?.is_some() => ControlGuard::load(store, public_key, None),
            _ => Ok(ControlGuard::unlinked(public_key)),
        };
    };
    let network = match env.network() {
        ChainNetwork::Mapped(network) => network,
        ChainNetwork::Unset => {
            bail!("refusing to sign: {MARKER_ENV} is set but {CHAIN_ENV} is not")
        }
        ChainNetwork::Unmapped(chain) => {
            bail!("refusing to sign: {MARKER_ENV} is set but {CHAIN_ENV}={chain} is not a control network")
        }
    };
    ensure!(
        network == marker.network,
        "refusing to sign: {MARKER_ENV} names {} but {CHAIN_ENV} is {network}",
        marker.network
    );
    let base = env.base().context("refusing to sign: no control directory")?;
    let store = find_store(network, &public_key, env)?.ok_or_else(|| {
        anyhow!(
            "refusing to sign: this agent is linked ({MARKER_ENV}={marker}) but its control \
             store under {} is missing: run `cloud-agent control restore`",
            base.display()
        )
    })?;
    ControlGuard::load(store, public_key, Some(&marker)).context("refusing to sign")
}

/// The signer for `key`, under the guard installed for it, else the one [`discover`] finds.
pub fn resolve(key: &Secret<32>) -> Result<AgentSigner> {
    resolve_with(key, &ControlEnv::from_process())
}

/// [`resolve`] with explicit control settings instead of the process environment.
pub fn resolve_with(key: &Secret<32>, env: &ControlEnv) -> Result<AgentSigner> {
    let verifying_key = use_key(key, None)?.verifying_key;
    let public_key = verifying_key.to_bytes();
    let guard = match installed(&public_key) {
        Some(guard) => guard,
        None => install_or_existing(Arc::new(discover(public_key, env)?))?,
    };
    Ok(AgentSigner {
        key: key.clone(),
        verifying_key,
        guard,
    })
}

/// The quote signer for `quote_key`, under the guard of the agent key.
pub fn resolve_quote(quote_key: &AtomicQuoteKey, agent_key: &Secret<32>) -> Result<QuoteSigner> {
    Ok(QuoteSigner::new(
        quote_key.clone(),
        Arc::clone(resolve(agent_key)?.guard()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: [u8; 32] = [
        0x0f, 0xe6, 0x65, 0xf7, 0xed, 0xb1, 0x93, 0xdb, 0x35, 0xcc, 0x37, 0xd7, 0xd7, 0x03, 0xe1,
        0x2a, 0xe9, 0x4e, 0x9e, 0x1c, 0x5f, 0x5b, 0x88, 0x57, 0xae, 0x1b, 0x6a, 0xca, 0x00, 0x5d,
        0xf1, 0x5b,
    ];

    fn signer_for(seed: [u8; 32]) -> AgentSigner {
        resolve(&Secret::seal(&mut { seed }).unwrap()).unwrap()
    }

    async fn unrestricted(signer: &AgentSigner) -> Authorization {
        signer.authorize(&[]).await.unwrap()
    }

    /// The JWT builder as it was before the seam.
    fn legacy_jwt(
        party_id: &str,
        role: &str,
        seed: &[u8; 32],
        ttl_secs: u64,
        node_name: Option<&str>,
        venue_branch: Option<&str>,
        now: u64,
    ) -> String {
        let mut claims = serde_json::json!({
            "sub": party_id,
            "exp": now.saturating_add(ttl_secs),
            "iat": now,
            "role": role,
        });
        if let Some(obj) = claims.as_object_mut() {
            if let Some(nn) = node_name {
                obj.insert(
                    "node_name".to_string(),
                    serde_json::Value::String(nn.to_string()),
                );
            }
            if let Some(branch) = venue_branch.filter(|b| !b.is_empty()) {
                obj.insert(
                    "venue_branch".to_string(),
                    serde_json::Value::String(branch.to_string()),
                );
            }
        }
        let signing_key = SigningKey::from_bytes(seed);
        let public_key = signing_key.verifying_key();
        let header = serde_json::json!({
            "typ": "JWT",
            "alg": "EdDSA",
            "jwk": {"kty": "OKP", "crv": "Ed25519", "x": URL_SAFE_NO_PAD.encode(public_key.as_bytes())}
        });
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_string(&header).unwrap().as_bytes());
        let claims_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_string(&claims).unwrap().as_bytes());
        let signing_input = format!("{header_b64}.{claims_b64}");
        let signature = signing_key.sign(signing_input.as_bytes());
        format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }

    /// The transaction-hash signer as it was before the seam.
    fn legacy_sign_hash(seed: &[u8; 32], hash: &[u8]) -> String {
        STANDARD.encode(SigningKey::from_bytes(seed).sign(hash).to_bytes())
    }

    fn iat_of(jwt: &str) -> u64 {
        let claims = jwt.split('.').nth(1).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
        claims["iat"].as_u64().unwrap()
    }

    fn assert_same(new: &MessageSignatureData, old: &MessageSignatureData) {
        assert_eq!(new.signature_b64, old.signature_b64);
        assert_eq!(new.public_key_b64url, old.public_key_b64url);
        assert_eq!(new.signing_scheme, old.signing_scheme);
    }

    /// One canonical per request kind, from the builders the agent uses.
    fn request_canonicals() -> Vec<(RequestKind, Vec<u8>)> {
        use message_signing::*;
        vec![
            (
                RequestKind::PrepareTransaction,
                canonical_prepare_request(
                    3,
                    &canonical_params_transfer_cc(
                        "bob::1220ab",
                        "1.5",
                        Some("memo"),
                        "cmd-1",
                        None,
                    ),
                ),
            ),
            (
                RequestKind::PrepareTransaction,
                canonical_prepare_request(
                    0,
                    &canonical_params_faucet("CC", "dso::1220", "t-1", false),
                ),
            ),
            (
                RequestKind::PrepareAtomicTransaction,
                canonical_prepare_atomic_request(&canonical_params_issue_tickets(&[
                    "t1".to_string()
                ])),
            ),
            (
                RequestKind::ExecuteTransaction,
                canonical_execute_request("tx-1", "c2ln"),
            ),
            (
                RequestKind::PreparePayFee,
                canonical_prepare_pay_fee_request("p1", "dvp"),
            ),
            (
                RequestKind::ExecutePayFee,
                canonical_execute_pay_fee_request("p1", "dvp", "[]", "s1"),
            ),
            (
                RequestKind::RegisterAgent,
                canonical_register_agent("pk", Some("AGT-code")),
            ),
            (
                RequestKind::GetOnboardingStatus,
                canonical_get_onboarding_status("pk"),
            ),
            (
                RequestKind::SubmitOnboardingSignature,
                canonical_submit_onboarding_signature("pk", "c2ln"),
            ),
        ]
    }

    #[tokio::test]
    async fn unlinked_signer_reproduces_todays_bytes() {
        let signer = signer_for(SEED);
        let auth = unrestricted(&signer).await;

        // JWTs, with and without the optional claims, and through the auth.rs wrappers.
        let now = 1_763_800_000;
        for (node, branch) in [
            (None, None),
            (Some("node-1"), Some("lp")),
            (Some("node-1"), Some("")),
        ] {
            let req = JwtRequest {
                sub: "agent::1220ab",
                role: "agent",
                ttl_secs: 3600,
                node_name: node,
                venue_branch: branch,
            };
            assert_eq!(
                signer.sign_jwt_at(&req, now).unwrap(),
                legacy_jwt("agent::1220ab", "agent", &SEED, 3600, node, branch, now)
            );
        }
        let wrapped = crate::auth::generate_jwt_with_branch(
            "p::1",
            "trader",
            &SEED,
            60,
            Some("n"),
            Some("agent"),
        )
        .unwrap();
        assert_eq!(
            wrapped,
            legacy_jwt(
                "p::1",
                "trader",
                &SEED,
                60,
                Some("n"),
                Some("agent"),
                iat_of(&wrapped)
            )
        );
        let wrapped = crate::auth::generate_jwt("p::1", "trader", &SEED, 60, None).unwrap();
        assert_eq!(
            wrapped,
            legacy_jwt("p::1", "trader", &SEED, 60, None, None, iat_of(&wrapped))
        );

        // Request signatures, one per kind.
        for (kind, canonical) in request_canonicals() {
            assert_eq!(RequestKind::of(&canonical), Some(kind));
            assert_same(
                &signer.sign_request(&canonical).unwrap(),
                &message_signing::sign_canonical(&SEED, &canonical),
            );
        }

        // Fee consents.
        let tx = FeeConsent::Transaction {
            party_id: "agent::1220ab",
            transaction_id: "tx-1",
            fees_json: "[{\"amount\":\"1\"}]",
        };
        let old = message_signing::sign_canonical(
            &SEED,
            &message_signing::canonical_tx_fees_authorization(
                "agent::1220ab",
                "tx-1",
                "[{\"amount\":\"1\"}]",
            ),
        );
        assert_same(&signer.sign_fee_consent(&auth, &tx).unwrap(), &old);
        let pay = FeeConsent::PayFee {
            party_id: "agent::1220ab",
            session_id: "s1",
            proposal_id: "p1",
            fee_type: "dvp",
            fees_json: "[]",
        };
        let old = message_signing::sign_canonical(
            &SEED,
            &message_signing::canonical_pay_fee_authorization(
                "agent::1220ab",
                "s1",
                "p1",
                "dvp",
                "[]",
            ),
        );
        assert_same(&signer.sign_fee_consent(&auth, &pay).unwrap(), &old);

        // Orders: the same canonical JSON and signature as before.
        let order = OrderToSign {
            market_id: "BTC-USD",
            order_type: "bid",
            price: "100.50",
            quantity: "1.0",
            nonce: 1_763_800_000_123,
        };
        let signed = signer.sign_order(&order).unwrap();
        let mut fields = BTreeMap::new();
        fields.insert(
            "market_id",
            serde_json::Value::String("BTC-USD".to_string()),
        );
        fields.insert("nonce", serde_json::json!(1_763_800_000_123u64));
        fields.insert("order_type", serde_json::Value::String("bid".to_string()));
        fields.insert("placed_by", serde_json::Value::String("agent".to_string()));
        fields.insert("price", serde_json::Value::String("100.50".to_string()));
        fields.insert("quantity", serde_json::Value::String("1.0".to_string()));
        let old_data = serde_json::to_vec(&fields).unwrap();
        assert_eq!(signed.signed_data, old_data);
        assert_eq!(
            signed.signature,
            crate::auth::sign_order_data(&SEED, &old_data)
        );
        assert!(crate::auth::verify_order_signature_with_public_key(
            signer.verifying_key(),
            &signed.signed_data,
            &signed.signature
        ));

        // Transaction hashes, computed locally or from the server.
        let hash = [0x5au8; 32];
        let expected = legacy_sign_hash(&SEED, &hash);
        assert_eq!(
            signer
                .sign_transaction(&auth, &TxHash::self_computed(hash).unwrap())
                .unwrap(),
            expected
        );
        assert_eq!(
            signer
                .sign_transaction(&auth, &TxHash::server_fallback(&hash).unwrap())
                .unwrap(),
            expected
        );

        // The onboarding multihash and the `sign` command.
        let multihash = [0x12u8; ONBOARDING_MULTIHASH_LEN];
        assert_eq!(
            signer.sign_onboarding_multihash(&multihash).unwrap(),
            legacy_sign_hash(&SEED, &multihash)
        );
        assert_eq!(
            signer.sign_raw_cli(b"hello").unwrap(),
            legacy_sign_hash(&SEED, b"hello")
        );
        assert_eq!(
            signer.public_key(),
            SigningKey::from_bytes(&SEED).verifying_key().to_bytes()
        );
    }

    #[tokio::test]
    async fn sign_transaction_requires_32_non_zero_bytes() {
        let signer = signer_for([21u8; 32]);
        let auth = unrestricted(&signer).await;
        for bad in [&[1u8; 31][..], &[1u8; 33][..], &[][..], &[0u8; 32][..]] {
            let err = TxHash::server_fallback(bad).unwrap_err().to_string();
            assert!(err.contains("32 non-zero bytes"), "{err}");
        }
        assert!(TxHash::self_computed([0u8; 32]).is_err());
        // A hash built without the constructors is checked again before signing.
        for zero in [
            TxHash::SelfComputed([0u8; 32]),
            TxHash::ServerFallback([0u8; 32]),
        ] {
            let err = signer
                .sign_transaction(&auth, &zero)
                .unwrap_err()
                .to_string();
            assert!(err.contains("32 non-zero bytes"), "{err}");
        }
        let fallback = TxHash::server_fallback(&[1u8; 32]).unwrap();
        assert!(fallback.is_server_fallback() && !signer.guard().strict());
        assert!(
            signer.sign_transaction(&auth, &fallback).is_ok(),
            "allowed while unlinked"
        );
    }

    #[tokio::test]
    async fn requests_are_an_allowlist_and_never_fee_authorizations() {
        let signer = signer_for([22u8; 32]);
        let fee_canonicals = [
            message_signing::canonical_tx_fees_authorization("p", "tx-1", "[]"),
            message_signing::canonical_pay_fee_authorization("p", "s1", "p1", "dvp", "[]"),
            message_signing::canonical_off_chain_authorization("p", "n1", "kind", "ext", "[]"),
        ];
        for canonical in &fee_canonicals {
            let err = signer.sign_request(canonical).err().unwrap().to_string();
            assert!(err.contains("not a request"), "{err}");
        }
        let foreign: [&[u8]; 8] = [
            b"",
            b"hello",
            b"msg_type=RegisterAgent",
            b"msg_type=RegisterAgentX\npublic_key=pk\n",
            b"xmsg_type=RegisterAgent\npublic_key=pk\n",
            b"msg_type=PrepareTransactionResponse\ntransaction_id=t\n",
            b"msg_type=ExecuteTransactionResponse\nsuccess=true\n",
            b"msg_type=silvana.agent.control.v1\nseq=1\n",
        ];
        for canonical in foreign {
            assert!(
                signer.sign_request(canonical).is_err(),
                "{}",
                String::from_utf8_lossy(canonical)
            );
        }
        // Fee consents pass only as their own intent, and requests never do.
        let auth = unrestricted(&signer).await;
        for canonical in &fee_canonicals[..2] {
            assert!(
                signer
                    .guard()
                    .check(&SigningIntent::FeeConsent {
                        canonical,
                        authorization: &auth
                    })
                    .is_ok()
            );
        }
        for canonical in [&fee_canonicals[2], &request_canonicals()[0].1] {
            let err = signer
                .guard()
                .check(&SigningIntent::FeeConsent {
                    canonical,
                    authorization: &auth,
                })
                .unwrap_err();
            assert!(err.to_string().contains("not a fee authorization"), "{err}");
        }
    }

    #[tokio::test]
    async fn the_onboarding_multihash_is_34_bytes() {
        let signer = signer_for([23u8; 32]);
        for len in [0usize, 32, 33, 35, 64] {
            let err = signer
                .sign_onboarding_multihash(&vec![0x12u8; len])
                .unwrap_err()
                .to_string();
            assert!(err.contains("34 bytes"), "{len}: {err}");
        }
        assert!(signer.sign_onboarding_multihash(&[0x12u8; 34]).is_ok());
    }

    #[tokio::test]
    async fn raw_cli_refuses_what_other_intents_sign() {
        let signer = signer_for([25u8; 32]);
        let env = ControlEnv { control_dir: Some(temp_base("raw-cli").into_os_string()), ..ControlEnv::default() };
        let control = b"scheme=silvana.agent.control.v1\nnetwork=devnet\n".to_vec();
        let jwt_input = jwt::signing_input(&signer.public_key(), &JwtRequest { sub: "a::1220ab", role: "agent", ttl_secs: 60, node_name: None, venue_branch: None }, 1).unwrap();
        for (bytes, expected) in [
            (vec![7u8; 32], "transaction hash"),
            (control, "control document"),
            (b"scheme=silvana.agent.policy.v1\n".to_vec(), "control document"),
            (b"msg_type=PrepareTransactionRequest\nx=1\n".to_vec(), "request or authorization"),
            (b"msg_type=anything".to_vec(), "request or authorization"),
            (jwt_input.into_bytes(), "JWT"),
            (b"eyJhbGciOi.e30".to_vec(), "JWT"),
        ] {
            let err = signer.sign_raw_cli_with(&bytes, &env).unwrap_err().to_string();
            assert!(err.contains(expected), "{expected}: {err}");
        }
        // Other bytes sign as before; the multihash command keeps its 34 bytes.
        for bytes in [&b"hello"[..], &[7u8; 31], &[7u8; 33], b"eyJ-no-dot", b"scheme=other"] {
            assert!(signer.sign_raw_cli_with(bytes, &env).is_ok(), "{bytes:?}");
        }
        let multihash = STANDARD.encode([0x12u8; ONBOARDING_MULTIHASH_LEN]);
        assert!(crate::sign::sign_multihash(&signer, &multihash).is_ok());
    }

    #[tokio::test]
    async fn raw_cli_is_refused_when_the_default_control_dir_holds_this_keys_store() {
        // A shell without the agent's env: no marker, no chain, so the guard reads unlinked.
        let seed = [0x26u8; 32];
        let (base, store, _) = stored_chain(seed, 1, "raw-cli-store").await;
        let signer = signer_with(seed, ControlGuard::unlinked(public(seed)));
        let env = ControlEnv { control_dir: Some(base.clone().into_os_string()), ..ControlEnv::default() };
        let err = signer.sign_raw_cli_with(b"hello", &env).unwrap_err().to_string();
        assert!(err.contains("control store") && err.contains("refused for a linked agent"), "{err}");
        // A pending first entry counts too; another key's store does not.
        std::fs::remove_dir_all(store.dir()).unwrap();
        assert!(signer.sign_raw_cli_with(b"hello", &env).is_ok());
        let party = party_for(seed, "agent-1").unwrap();
        let mut chain = TestChain::new(seed, &party, Network::Mainnet).unwrap();
        let link = chain.link_pending(0x21).unwrap();
        let mainnet = ControlStore::open(&base, Network::Mainnet, &party);
        let lock = mainnet.lock().await.unwrap();
        mainnet.record_pending(&lock, &chain.push(&link).unwrap(), None, 1).unwrap();
        drop(lock);
        assert!(signer.sign_raw_cli_with(b"hello", &env).unwrap_err().to_string().contains("control store"));
        let other = signer_for([0x27u8; 32]);
        assert!(other.sign_raw_cli_with(b"hello", &env).is_ok());
        // No control directory at all: nothing to find.
        assert!(signer.sign_raw_cli_with(b"hello", &ControlEnv::default()).is_ok());
    }

    #[tokio::test]
    async fn jwt_and_order_intents_sign_only_their_own_shape() {
        let signer = signer_for([24u8; 32]);
        for input in [&b"abc"[..], b"a.b.c", b"e30.e30"] {
            assert!(signer.guard().check(&SigningIntent::Jwt(input)).is_err());
        }
        // A token carrying another key's jwk is refused.
        let other = signer_for([25u8; 32]);
        let req = JwtRequest {
            sub: "p",
            role: "agent",
            ttl_secs: 60,
            node_name: None,
            venue_branch: None,
        };
        let foreign = jwt::signing_input(&other.public_key(), &req, 1).unwrap();
        assert!(
            signer
                .guard()
                .check(&SigningIntent::Jwt(foreign.as_bytes()))
                .is_err()
        );
        let own = jwt::signing_input(&signer.public_key(), &req, 1).unwrap();
        assert!(
            signer
                .guard()
                .check(&SigningIntent::Jwt(own.as_bytes()))
                .is_ok()
        );
        for data in [&b"{\"nonce\":1}"[..], b"[]", b"{\"market_id\":\"x\""] {
            assert!(signer.guard().check(&SigningIntent::Order(data)).is_err());
        }
    }

    #[tokio::test]
    async fn an_authorization_works_only_with_its_own_guard() {
        let signer = signer_for([26u8; 32]);
        let other = signer_for([27u8; 32]);
        let foreign = unrestricted(&other).await;
        assert!(foreign.is_unrestricted());
        let err = signer
            .sign_transaction(&foreign, &TxHash::self_computed([1u8; 32]).unwrap())
            .unwrap_err();
        assert!(err.to_string().contains("another key"), "{err}");
        let consent = FeeConsent::Transaction {
            party_id: "p",
            transaction_id: "t",
            fees_json: "[]",
        };
        assert!(signer.sign_fee_consent(&foreign, &consent).is_err());
        signer.release(unrestricted(&signer).await).await;
    }

    #[test]
    fn one_guard_per_key_and_install_refuses_another() {
        let a = signer_for([28u8; 32]);
        let b = signer_for([28u8; 32]);
        assert!(Arc::ptr_eq(a.guard(), b.guard()));
        assert!(install(Arc::clone(a.guard())).is_ok());
        let err = install(Arc::new(ControlGuard::unlinked(a.public_key()))).unwrap_err();
        assert!(err.to_string().contains("already installed"), "{err}");
        let fresh = Arc::new(ControlGuard::unlinked([0xEEu8; 32]));
        assert!(install(Arc::clone(&fresh)).is_ok());
        assert!(!fresh.is_linked() && !fresh.strict());
    }

    #[test]
    fn a_key_that_cannot_be_opened_is_an_error() {
        let err = resolve(&Secret::corrupt_for_tests()).unwrap_err();
        assert_eq!(err.to_string(), "sealed secret is corrupt");
        let corrupt = AgentSigner::corrupt_for_tests();
        let err = corrupt.sign_raw_cli(b"x").unwrap_err();
        assert_eq!(err.to_string(), "sealed secret is corrupt");
        // The rules run before the key is opened.
        let err = corrupt
            .sign_request(b"msg_type=TxFeesAuthorization\nparty_id=p\n")
            .err()
            .unwrap();
        assert!(err.to_string().contains("not a request"), "{err}");
        let from_b58 = AgentSigner::from_base58(&bs58::encode(SEED).into_string()).unwrap();
        assert_eq!(from_b58.public_key(), signer_for(SEED).public_key());
    }

    #[tokio::test]
    async fn the_quote_signer_signs_only_its_own_quote() {
        let kf = atomic_quote::gen_keypair().unwrap();
        let key = AtomicQuoteKey::from_scalar_hex(&kf.priv_scalar_hex).unwrap();
        let quote = resolve_quote(&key, &Secret::seal(&mut [29u8; 32]).unwrap()).unwrap();
        assert!(Arc::ptr_eq(quote.guard(), signer_for([29u8; 32]).guard()));
        let canonical =
            "msg_type=silvana.atomic-dvp.quote.v3\nlp=lp\nquote_nonce=q-1\nticket_id=\n";
        let intent = QuoteIntent {
            quote_id: "q-1",
            market_id: "M",
            side: "Sell",
            base_amount: "1.0",
            quote_amount: "2.0",
            user_party: "u",
            lp_fee: None,
        };
        let signature = quote.authorize_and_sign(&intent, canonical).await.unwrap();
        assert_eq!(
            signature,
            atomic_quote::sign_quote(&kf.priv_scalar_hex, canonical).unwrap()
        );
        assert!(atomic_quote::verify_quote(
            &signature,
            canonical,
            quote.pub_spki_hex()
        ));
        let other = QuoteIntent {
            quote_id: "q-2",
            ..intent
        };
        assert!(quote.authorize_and_sign(&other, canonical).await.is_err());
        assert!(
            quote
                .authorize_and_sign(&intent, "quote_nonce=q-1\n")
                .await
                .is_err()
        );
    }

    // ---- linked agents ----

    use crate::control_store::testing::{TestChain, party_for};
    use crate::control_store::{ControlEnv, ControlMarker, ControlStore, HEAD_FILE, LOG_FILE};
    use agent_control::Network;
    use std::path::PathBuf;

    fn temp_base(tag: &str) -> PathBuf {
        let mut bytes = [0u8; 8];
        let _ = rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
        let dir = std::env::temp_dir().join(format!("signer-{tag}-{}", hex::encode(bytes)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn public(seed: [u8; 32]) -> [u8; 32] {
        SigningKey::from_bytes(&seed).verifying_key().to_bytes()
    }

    /// A store under a fresh base holding the first `n` entries of a test chain.
    async fn stored_chain(seed: [u8; 32], n: usize, tag: &str) -> (PathBuf, ControlStore, TestChain) {
        let party = party_for(seed, "agent-1").unwrap();
        let base = temp_base(tag);
        let store = ControlStore::open(&base, Network::Devnet, &party);
        let chain = TestChain::build(seed, &party, Network::Devnet, n).unwrap();
        let lock = store.lock().await.unwrap();
        store.append(&lock, &chain.entries, &[]).unwrap();
        (base, store, chain)
    }

    fn signer_with(seed: [u8; 32], guard: ControlGuard) -> AgentSigner {
        AgentSigner {
            key: Secret::seal(&mut { seed }).unwrap(),
            verifying_key: SigningKey::from_bytes(&seed).verifying_key(),
            guard: Arc::new(guard),
        }
    }

    async fn linked_signer(seed: [u8; 32], n: usize, tag: &str) -> (AgentSigner, ControlStore, TestChain) {
        let (_, store, chain) = stored_chain(seed, n, tag).await;
        let guard = ControlGuard::load(store.clone(), public(seed), None).unwrap();
        (signer_with(seed, guard), store, chain)
    }

    fn claims_of(jwt: &str) -> serde_json::Value {
        let claims = jwt.split('.').nth(1).unwrap();
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_linked_guard_refuses_every_bypass() {
        let (signer, _, chain) = linked_signer([0x60; 32], 2, "bypass").await;
        let guard = signer.guard();
        assert!(guard.is_linked() && guard.strict());
        assert_eq!(guard.head(), chain.state.head());
        let auth = unrestricted(&signer).await;
        let err = signer
            .sign_transaction(&auth, &TxHash::server_fallback(&[1u8; 32]).unwrap())
            .unwrap_err();
        assert!(err.to_string().contains("not computed locally"), "{err}");
        assert!(signer.sign_transaction(&auth, &TxHash::self_computed([1u8; 32]).unwrap()).is_ok());
        for err in [
            signer.sign_raw_cli(b"hello").unwrap_err(),
            signer.sign_onboarding_multihash(&[0x12u8; ONBOARDING_MULTIHASH_LEN]).unwrap_err(),
        ] {
            assert!(err.to_string().contains("not allowed for a linked agent"), "{err}");
        }
        // Requests and fee consents sign as before.
        for (_, canonical) in request_canonicals() {
            assert!(signer.sign_request(&canonical).is_ok());
        }
        let consent = FeeConsent::Transaction { party_id: "p", transaction_id: "t", fees_json: "[]" };
        assert!(signer.sign_fee_consent(&auth, &consent).is_ok());
    }

    #[tokio::test]
    async fn receipts_and_head_claims_sign_only_this_agents_crate_documents() {
        use agent_control::{
            CodeSource, Hash32, HeadClaim, Receipt, ReceiptOutcome, ReceiptSubject, UtcTime,
            verify_ed25519_strict,
        };
        let seed = [0x6a; 32];
        let (signer, _, chain) = linked_signer(seed, 1, "receipt").await;
        let at = UtcTime::from_unix_secs(1_795_000_600).unwrap();
        let claim = HeadClaim::for_state(&chain.state, at);
        let signature = signer.sign_head_claim(&claim).unwrap();
        verify_ed25519_strict(&public(seed), &claim.encode().unwrap(), &signature).unwrap();
        let stale = HeadClaim { head: None, ..claim.clone() };
        assert!(signer.sign_head_claim(&stale).unwrap_err().to_string().contains("not the accepted head"));
        let receipt = Receipt {
            network: Network::Devnet,
            agent_party: chain.state.agent_party().clone(),
            agent_key: public(seed),
            subject: ReceiptSubject::Policy(Hash32([7; 32])),
            subject_hash: Hash32([7; 32]),
            outcome: ReceiptOutcome::Refused,
            update_id: None,
            reason_code: Some(4117),
            code_source: Some(CodeSource::Agent),
            log_seq: 1,
            issued_at: at,
        };
        let signature = signer.sign_receipt(&receipt).unwrap();
        verify_ed25519_strict(&public(seed), &receipt.encode().unwrap(), &signature).unwrap();
        let ahead = Receipt { log_seq: 2, ..receipt.clone() };
        assert!(signer.sign_receipt(&ahead).unwrap_err().to_string().contains("beyond the accepted head"));
        let foreign = Receipt { agent_key: public([0x6b; 32]), ..receipt.clone() };
        assert!(signer.sign_receipt(&foreign).is_err());
        // An unlinked agent claims only an empty log and signs no receipt.
        let unlinked = signer_for([0x6c; 32]);
        let mut empty = HeadClaim::for_state(&chain.state, at);
        empty.agent_key = public([0x6c; 32]);
        assert!(unlinked.sign_head_claim(&empty).is_err());
        empty.head = None;
        assert!(unlinked.sign_head_claim(&empty).is_ok());
        let theirs = Receipt { agent_key: public([0x6c; 32]), ..receipt };
        assert!(unlinked.sign_receipt(&theirs).unwrap_err().to_string().contains("only a linked agent"));
    }

    #[tokio::test]
    async fn a_v2_receipt_signs_only_for_an_executed_command_of_this_agent() {
        use agent_control::{
            Amount, AnyReceipt, Hash32, PartyId, Receipt, ReceiptOutcome, ReceiptSubject, SwapReceipt, SwapResult,
            Token, UtcTime, Uuid7, verify_ed25519_strict,
        };
        let seed = [0x6d; 32];
        let (signer, _, chain) = linked_signer(seed, 1, "receipt-v2").await;
        let at = UtcTime::from_unix_secs(1_795_000_600).unwrap();
        let receipt = Receipt {
            network: Network::Devnet,
            agent_party: chain.state.agent_party().clone(),
            agent_key: public(seed),
            subject: ReceiptSubject::Command(Uuid7::parse("01890a5d-ac96-774b-bcce-b302099a8057").unwrap()),
            subject_hash: Hash32([7; 32]),
            outcome: ReceiptOutcome::Executed,
            update_id: Some(Token::parse("1220update").unwrap()),
            reason_code: None,
            code_source: None,
            log_seq: 1,
            issued_at: at,
        };
        let result = SwapResult {
            quote_id: Token::parse("q-1").unwrap(),
            lp: PartyId::parse(&format!("lp::1220{}", "ab".repeat(32))).unwrap(),
            sold: Amount::parse("10").unwrap(),
            received: Amount::parse("2.5").unwrap(),
            fee_cc: Amount::parse("0").unwrap(),
        };
        let v2 = AnyReceipt::V2(SwapReceipt { receipt: receipt.clone(), result: result.clone() });
        let signature = signer.sign_any_receipt(&v2).unwrap();
        verify_ed25519_strict(&public(seed), &v2.encode().unwrap(), &signature).unwrap();
        // v1 goes through the same path.
        assert!(signer.sign_any_receipt(&AnyReceipt::V1(receipt.clone())).is_ok());
        // A v2 receipt of a refusal, or of a policy, does not even encode.
        let refused = Receipt {
            outcome: ReceiptOutcome::Refused,
            update_id: None,
            reason_code: Some(1106),
            code_source: Some(agent_control::CodeSource::Agent),
            ..receipt.clone()
        };
        let not_executed = AnyReceipt::V2(SwapReceipt { receipt: refused, result: result.clone() });
        assert!(signer.sign_any_receipt(&not_executed).unwrap_err().to_string().contains("does not encode"));
        // Another agent's v2 receipt is refused by the guard.
        let foreign = SwapReceipt { receipt: Receipt { agent_key: public([0x6e; 32]), ..receipt }, result };
        let err = signer.sign_any_receipt(&AnyReceipt::V2(foreign)).unwrap_err();
        assert!(err.to_string().contains("another agent"), "{err}");
    }

    #[tokio::test]
    async fn jwt_head_claims_only_when_linked() {
        let unlinked = signer_for([0x61; 32]);
        let party = party_for([0x61; 32], "agent-1").unwrap();
        let req = JwtRequest { sub: party.as_str(), role: "agent", ttl_secs: 3600, node_name: None, venue_branch: None };
        let (token, head) = unlinked.sign_jwt_with_head(&req).unwrap();
        assert_eq!(head, None);
        let claims = claims_of(&token);
        assert!(claims.get("ctl_seq").is_none() && claims.get("ctl_head").is_none(), "{claims}");

        let (linked, _, chain) = linked_signer([0x62; 32], 2, "jwt").await;
        let party = chain.state.agent_party().clone();
        let req = JwtRequest { sub: party.as_str(), ..req };
        let (token, head) = linked.sign_jwt_with_head(&req).unwrap();
        let expected = chain.state.head().unwrap();
        assert_eq!(head, Some(expected));
        let claims = claims_of(&token);
        assert!(claims["ctl_seq"].is_u64(), "{claims}");
        assert_eq!(claims["ctl_seq"].as_u64(), Some(expected.seq));
        let ctl_head = claims["ctl_head"].as_str().unwrap();
        assert_eq!(ctl_head.len(), 64);
        assert!(ctl_head.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(ctl_head, expected.hash.to_string());
        assert!(crate::auth::verify_order_signature_with_public_key(
            linked.verifying_key(),
            token.rsplit_once('.').unwrap().0.as_bytes(),
            &STANDARD.encode(URL_SAFE_NO_PAD.decode(token.rsplit('.').next().unwrap()).unwrap()),
        ));
        // A linked agent signs tokens only for its own party.
        let other = JwtRequest { sub: "someone::1220ab", ..req };
        let err = linked.sign_jwt(&other).unwrap_err();
        assert!(err.to_string().contains("only for its own party"), "{err}");
        // Control-service tokens live at most 900 s and name the control services of one network.
        for (ttl, expected_ttl) in [(3600, 900), (900, 900), (60, 60)] {
            let (token, _) = linked.sign_control_jwt(&JwtRequest { ttl_secs: ttl, ..req }, Network::Devnet).unwrap();
            let c = claims_of(&token);
            assert_eq!(c["exp"].as_u64().unwrap() - c["iat"].as_u64().unwrap(), expected_ttl);
            assert_eq!(c["ctl_seq"].as_u64(), Some(expected.seq));
            assert_eq!(c["aud"].as_str(), Some("silvana.agent.control.devnet"));
        }
        let (token, _) = linked.sign_control_jwt(&req, Network::Mainnet).unwrap();
        assert_eq!(claims_of(&token)["aud"].as_str(), Some("silvana.agent.control.mainnet"));
        assert_eq!(control_audience(Network::Devnet), format!("{}devnet", agent_control::CONTROL_TOKEN_AUDIENCE_PREFIX));
        // Every other token carries no audience, as before.
        assert!(claims_of(&linked.sign_jwt(&req).unwrap()).get("aud").is_none());
        assert!(claims_of(&unlinked.sign_jwt(&JwtRequest { sub: "x::1220ab", ..req }).unwrap()).get("aud").is_none());
        assert_eq!(control_ttl_secs(u64::MAX), CONTROL_TOKEN_MAX_TTL_SECS);
        // The guard's token path never claims a head beyond the accepted one.
        let ahead = JwtRequest { ttl_secs: 60, ..req };
        let input = jwt::signing_input_with_head(&linked.public_key(), &ahead, 1, Some(&HeadRef { seq: 3, ..expected })).unwrap();
        assert!(linked.sign(&SigningIntent::Jwt(input.as_bytes())).unwrap_err().to_string().contains("control head"));
    }

    #[tokio::test]
    async fn an_append_token_claims_only_a_head_the_store_holds() {
        use agent_control::Hash32;
        // Unlinked: the recorded pending first entry is the one head it may claim.
        let seed = [0x63; 32];
        let party = party_for(seed, "agent-1").unwrap();
        let base = temp_base("append-token");
        let store = ControlStore::open(&base, Network::Devnet, &party);
        let unlinked = signer_with(seed, ControlGuard::unlinked(public(seed)));
        let req = JwtRequest { sub: party.as_str(), role: "agent", ttl_secs: 3600, node_name: None, venue_branch: None };
        let mut chain = TestChain::new(seed, &party, Network::Devnet).unwrap();
        let link = chain.link_pending(0x21).unwrap();
        let lock = store.lock().await.unwrap();
        let unknown = HeadRef { seq: 1, hash: Hash32([9; 32]) };
        let err = unlinked.sign_control_jwt_for_head(&req, &store, &lock, &unknown).unwrap_err();
        assert!(err.to_string().contains("does not hold head seq 1"), "{err}");
        let signed = store.sign_entry(&lock, &unlinked, &link, 1).unwrap();
        let head = HeadRef { seq: signed.seq(), hash: *signed.hash() };
        let token = unlinked.sign_control_jwt_for_head(&req, &store, &lock, &head).unwrap();
        let c = claims_of(&token);
        assert_eq!(c["ctl_seq"].as_u64(), Some(1));
        assert_eq!(c["ctl_head"].as_str(), Some(head.hash.to_string().as_str()));
        assert_eq!(c["exp"].as_u64().unwrap() - c["iat"].as_u64().unwrap(), CONTROL_TOKEN_MAX_TTL_SECS);
        assert_eq!(c["aud"].as_str(), Some("silvana.agent.control.devnet"), "the store's network");
        assert!(crate::auth::verify_order_signature_with_public_key(
            unlinked.verifying_key(),
            token.rsplit_once('.').unwrap().0.as_bytes(),
            &STANDARD.encode(URL_SAFE_NO_PAD.decode(token.rsplit('.').next().unwrap()).unwrap()),
        ));
        // A head token for another network's control services, or for no service, is refused.
        let held = unlinked.held_head(&store, &lock, &head).unwrap();
        for audience in [Some("silvana.agent.control.mainnet"), Some("orderbook-devnet"), None] {
            let input = jwt::signing_input_for(&unlinked.public_key(), &JwtRequest { ttl_secs: 60, ..req }, 1, Some(&head), audience).unwrap();
            let err = unlinked.sign_under(Rules::HeldHeadToken(unlinked.guard(), &held), &SigningIntent::Jwt(input.as_bytes())).unwrap_err();
            assert!(err.to_string().contains("only for the control services of devnet"), "{audience:?}: {err}");
        }
        assert!(unlinked.sign_control_jwt(&req, Network::Devnet).unwrap().1.is_none(), "other tokens claim nothing yet");
        let other = JwtRequest { sub: "someone::1220ab", ..req };
        let err = unlinked.sign_control_jwt_for_head(&other, &store, &lock, &head).unwrap_err();
        assert!(err.to_string().contains("own party"), "{err}");
        let foreign = ControlStore::open(&temp_base("foreign"), Network::Devnet, &party_for([0x64; 32], "agent-1").unwrap());
        let foreign_lock = foreign.lock().await.unwrap();
        let err = unlinked.sign_control_jwt_for_head(&req, &foreign, &foreign_lock, &head).unwrap_err();
        assert!(err.to_string().contains("another key"), "{err}");
        drop(lock);

        // Linked: any entry of the log, or the recorded pending next one.
        let (linked, store, mut chain) = linked_signer([0x65; 32], 2, "append-linked").await;
        let party = chain.state.agent_party().clone();
        let req = JwtRequest { sub: party.as_str(), ..req };
        let lock = store.lock().await.unwrap();
        for entry in chain.entries.clone() {
            let head = HeadRef { seq: entry.seq(), hash: *entry.hash() };
            let token = linked.sign_control_jwt_for_head(&req, &store, &lock, &head).unwrap();
            assert_eq!(claims_of(&token)["ctl_seq"].as_u64(), Some(entry.seq()));
        }
        let next = chain.add_pending(0x66).unwrap();
        let signed = linked.sign_control_entry(&next).unwrap();
        let ahead = HeadRef { seq: 3, hash: *signed.hash() };
        assert!(linked.sign_control_jwt_for_head(&req, &store, &lock, &ahead).is_err(), "not recorded yet");
        store.record_pending(&lock, &signed, None, 1).unwrap();
        let token = linked.sign_control_jwt_for_head(&req, &store, &lock, &ahead).unwrap();
        assert_eq!(claims_of(&token)["ctl_seq"].as_u64(), Some(3));
        let (token, head) = linked.sign_control_jwt(&req, Network::Devnet).unwrap();
        assert_eq!(head, chain.state.head());
        assert_eq!(claims_of(&token)["ctl_seq"].as_u64(), Some(2), "other tokens claim the accepted head");
        let (_, copy, _) = stored_chain([0x65; 32], 2, "append-copy").await;
        let copy_lock = copy.lock().await.unwrap();
        let err = linked.sign_control_jwt_for_head(&req, &copy, &copy_lock, &ahead).unwrap_err();
        assert!(err.to_string().contains("not the one in force"), "{err}");
    }

    #[tokio::test]
    async fn reload_moves_forward_only() {
        let seed = [0x63; 32];
        let (signer, store, mut chain) = linked_signer(seed, 1, "reload").await;
        let guard = Arc::clone(signer.guard());
        let first = guard.head().unwrap();
        let old_log = std::fs::read(store.dir().join(LOG_FILE)).unwrap();
        let old_head = std::fs::read(store.dir().join(HEAD_FILE)).unwrap();
        assert_eq!(guard.reload().await.unwrap(), Reload::Unchanged);

        // Another process (the CLI) appends: the running guard follows.
        let add = chain.add_pending(0x71).unwrap();
        let signed = chain.push(&add).unwrap();
        {
            let lock = store.lock().await.unwrap();
            store.append(&lock, &[signed], &[]).unwrap();
        }
        let second = chain.state.head().unwrap();
        assert_eq!(guard.reload().await.unwrap(), Reload::Advanced { from: Some(first), to: second });
        assert_eq!(guard.head(), Some(second));
        let party = chain.state.agent_party().clone();
        let req = JwtRequest { sub: party.as_str(), role: "agent", ttl_secs: 60, node_name: None, venue_branch: None };
        assert_eq!(claims_of(&signer.sign_jwt(&req).unwrap())["ctl_seq"].as_u64(), Some(2));

        // A store rolled back on disk never moves the guard back.
        std::fs::write(store.dir().join(LOG_FILE), &old_log).unwrap();
        std::fs::write(store.dir().join(HEAD_FILE), &old_head).unwrap();
        let err = guard.reload().await.unwrap_err();
        assert!(err.to_string().contains("behind the one in force"), "{err}");
        assert_eq!(guard.head(), Some(second));

        // A deleted store leaves the guard linked and strict.
        std::fs::remove_dir_all(store.dir()).unwrap();
        let err = guard.reload().await.unwrap_err();
        assert!(err.to_string().contains("stays linked"), "{err}");
        assert!(guard.is_linked() && guard.strict());

        // An unlinked guard links from a store and then follows it.
        let seed = [0x64; 32];
        let (_, store, _) = stored_chain(seed, 2, "relink").await;
        let unlinked = ControlGuard::unlinked(public(seed));
        assert_eq!(unlinked.reload().await.unwrap(), Reload::Unchanged);
        let reloaded = unlinked.reload_from(&store, None).await.unwrap();
        assert!(matches!(reloaded, Reload::Advanced { from: None, .. }), "{reloaded:?}");
        assert!(unlinked.is_linked());
        let elsewhere = ControlStore::open(&temp_base("elsewhere"), Network::Devnet, store.party());
        assert!(unlinked.reload_from(&elsewhere, None).await.is_err());
    }

    #[tokio::test]
    async fn discovery_moves_an_earlier_versions_store_into_the_agent_folder() {
        let seed = [0xe7; 32];
        let party = party_for(seed, "agent-1").unwrap();
        let root = temp_base("legacy");
        let legacy = root.join("home/.silvana-agent/devnet").join(party.as_str().replace("::", "__"));
        let old = ControlStore::open(&legacy, Network::Devnet, &party);
        let chain = TestChain::build(seed, &party, Network::Devnet, 1).unwrap();
        let lock = old.lock().await.unwrap();
        old.append(&lock, &chain.entries, &[]).unwrap();
        drop(lock);
        let marker = crate::control_store::ControlMarker {
            network: Network::Devnet,
            head: HeadRef { seq: 1, hash: *chain.entries[0].hash() },
        };
        let env = ControlEnv {
            home: Some(root.join("home").into_os_string()),
            chain: Some("devnet".into()),
            marker: Some(marker.to_string()),
            party: Some(party.to_string()),
            agent_dir: Some(root.join("agent")),
            ..ControlEnv::default()
        };
        // The sign command checks the old location too, before anything moved.
        let unlinked = signer_with(seed, ControlGuard::unlinked(public(seed)));
        let err = unlinked.sign_raw_cli_with(b"hello", &env).unwrap_err().to_string();
        assert!(err.contains(&*old.dir().to_string_lossy()), "{err}");
        assert!(!root.join("agent/control").exists());
        assert!(resolve_with(&Secret::seal(&mut { seed }).unwrap(), &env).unwrap().guard().is_linked());
        assert!(root.join("agent/control").join(crate::control_store::LOG_FILE).exists());
        assert!(old.dir().join(crate::control_store::LOG_FILE).exists(), "the old store stays");
        let err = unlinked.sign_raw_cli_with(b"hello", &env).unwrap_err().to_string();
        assert!(err.contains("refused for a linked agent"), "{err}");
    }

    #[tokio::test]
    async fn discovery_follows_the_linked_marker() {
        let env_for = |base: &std::path::Path, store: &ControlStore, marker: Option<String>| ControlEnv {
            control_dir: Some(base.as_os_str().to_owned()),
            home: None,
            chain: Some("devnet".into()),
            marker,
            party: Some(store.party().to_string()),
            agent_dir: None,
        };
        let marker_of = |chain: &TestChain| ControlMarker { network: Network::Devnet, head: chain.state.head().unwrap() }.to_string();
        let key_of = |seed: [u8; 32]| Secret::seal(&mut { seed }).unwrap();

        // At the marker: linked.
        let seed = [0x65; 32];
        let (base, store, chain) = stored_chain(seed, 2, "at-marker").await;
        let env = env_for(&base, &store, Some(marker_of(&chain)));
        let signer = resolve_with(&key_of(seed), &env).unwrap();
        assert!(signer.guard().is_linked());
        assert_eq!(signer.guard().head(), chain.state.head());

        // The control dir deleted: refused.
        let seed = [0x66; 32];
        let (base, store, chain) = stored_chain(seed, 1, "deleted").await;
        let env = env_for(&base, &store, Some(marker_of(&chain)));
        std::fs::remove_dir_all(store.dir()).unwrap();
        let err = format!("{:#}", resolve_with(&key_of(seed), &env).unwrap_err());
        assert!(err.contains("store") && err.contains("missing"), "{err}");
        // Also when PARTY_AGENT is unset and the store is looked up by key.
        let env = ControlEnv { party: None, ..env };
        assert!(resolve_with(&key_of(seed), &env).is_err());
        assert!(installed(&public(seed)).is_none(), "a refusal installs nothing");

        // The store behind the marker, or diverging from it: refused.
        let seed = [0x67; 32];
        let (base, store, mut chain) = stored_chain(seed, 1, "behind").await;
        let add = chain.add_pending(0x72).unwrap();
        chain.push(&add).unwrap();
        let env = env_for(&base, &store, Some(marker_of(&chain)));
        let err = format!("{:#}", resolve_with(&key_of(seed), &env).unwrap_err());
        assert!(err.contains("behind the linked marker"), "{err}");
        let forged = format!("devnet:1:{}", agent_control::Hash32([0x11; 32]));
        let env = env_for(&base, &store, Some(forged));
        let err = format!("{:#}", resolve_with(&key_of(seed), &env).unwrap_err());
        assert!(err.contains("diverges"), "{err}");

        // Unmapped or unset networks: refused while the marker is set.
        let env = env_for(&base, &store, Some(format!("devnet:1:{}", chain.entries[0].hash())));
        for chain_name in [None, Some("testnet".to_string()), Some("mainnet".to_string())] {
            let env = ControlEnv { chain: chain_name.clone(), ..env.clone() };
            assert!(resolve_with(&key_of(seed), &env).is_err(), "{chain_name:?}");
        }
        // A malformed marker is refused too.
        let env = ControlEnv { marker: Some("devnet:x".into()), ..env.clone() };
        assert!(resolve_with(&key_of(seed), &env).is_err());

        // No marker and no store: unlinked, as today.
        let seed = [0x68; 32];
        let base = temp_base("none");
        let env = ControlEnv { control_dir: Some(base.into_os_string()), chain: Some("devnet".into()), ..ControlEnv::default() };
        assert!(!resolve_with(&key_of(seed), &env).unwrap().guard().is_linked());

        // No marker but a stored log: linked.
        let seed = [0x69; 32];
        let (base, store, _) = stored_chain(seed, 1, "no-marker").await;
        let env = env_for(&base, &store, None);
        assert!(resolve_with(&key_of(seed), &env).unwrap().guard().is_linked());

        // The marker of another PARTY_AGENT key does not cover this one.
        let seed = [0x6a; 32];
        let (base, store, _) = stored_chain([0x6b; 32], 1, "other-key").await;
        let env = ControlEnv { marker: Some(format!("devnet:1:{}", agent_control::Hash32([1; 32]))), ..env_for(&base, &store, None) };
        assert!(!resolve_with(&key_of(seed), &env).unwrap().guard().is_linked());
    }

    #[tokio::test]
    async fn control_entries_are_signed_in_full_and_only_after_the_head() {
        let seed = [0x6c; 32];
        let party = party_for(seed, "agent-1").unwrap();
        let mut chain = TestChain::new(seed, &party, Network::Devnet).unwrap();
        let unlinked = signer_with(seed, ControlGuard::unlinked(public(seed)));
        let link = chain.link_pending(0x21).unwrap();
        let signed = unlinked.sign_control_entry(&link).unwrap();
        assert_eq!(signed, chain.push(&link).unwrap(), "PureEdDSA over the full entry");
        let add = chain.add_pending(0x73).unwrap();
        let err = unlinked.sign_control_entry(&add).unwrap_err();
        assert!(err.to_string().contains("only the first control entry"), "{err}");
        // Another key's entry is refused.
        let stranger = signer_with([0x6d; 32], ControlGuard::unlinked(public([0x6d; 32])));
        assert!(stranger.sign_control_entry(&link).is_err());

        let (linked, _, mut chain) = linked_signer([0x6e; 32], 1, "entries").await;
        let next = chain.add_pending(0x74).unwrap();
        assert!(linked.sign_control_entry(&next).is_ok());
        chain.push(&next).unwrap();
        let ahead = chain.add_pending(0x75).unwrap();
        let err = linked.sign_control_entry(&ahead).unwrap_err();
        assert!(err.to_string().contains("does not follow the head"), "{err}");
    }
}
