//! Receipts of commands, checked against the verified log: a receipt counts only when the
//! agent key signed it for this network, this command id and these command bytes.

use agent_control::{AnyReceipt, Hash32, ReceiptSubject, Uuid7};
use orderbook_proto::orderbook::ControlReceipt;
use sha2::Digest;

use crate::client::VerifiedAgent;

/// What a command's receipt comes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptCheck {
    /// The rpc holds no receipt yet.
    None,
    /// Signed by the agent key for exactly this command.
    Verified(Box<AnyReceipt>),
    /// Present but not provable; the text says why.
    Unverified(String),
}

impl ReceiptCheck {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }
}

/// Checks `receipt` for the command `id` whose signed bytes are `command`.
pub fn check_receipt(
    agent: &VerifiedAgent,
    id: &Uuid7,
    command: &[u8],
    receipt: Option<&ControlReceipt>,
) -> ReceiptCheck {
    let Some(receipt) = receipt else {
        return ReceiptCheck::None;
    };
    let Ok(signature) = <[u8; 64]>::try_from(receipt.signature.as_slice()) else {
        return ReceiptCheck::Unverified("its signature is not 64 bytes".into());
    };
    let log = agent.log();
    let decoded = match AnyReceipt::verify(&receipt.canonical, &signature, log.agent_party()) {
        Ok(decoded) => decoded,
        Err(e) => {
            return ReceiptCheck::Unverified(format!("it does not verify ({})", e.code_name()));
        }
    };
    let body = decoded.receipt();
    if body.network != log.network() {
        return ReceiptCheck::Unverified("it is for another network".into());
    }
    if body.agent_key != *log.agent_key() {
        return ReceiptCheck::Unverified("it is signed by another key than the log's".into());
    }
    if body.subject != ReceiptSubject::Command(id.clone()) {
        return ReceiptCheck::Unverified("it is about another command".into());
    }
    if body.subject_hash != Hash32(sha2::Sha256::digest(command).into()) {
        return ReceiptCheck::Unverified("it is about other command bytes".into());
    }
    ReceiptCheck::Verified(Box::new(decoded))
}
