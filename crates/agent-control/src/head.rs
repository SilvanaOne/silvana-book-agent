//! Agent-signed head claims (`silvana.agent.control-head.v1`).

use crate::SCHEME_HEAD;
use crate::canon::time::UtcTime;
use crate::canon::values::{Hash32, Network, parse_hex_array, parse_u64};
use crate::canon::{Reader, Writer, decode_exact};
use crate::error::{CanonError, CanonErrorKind, SignedDocError, ValueError, ValueKind};
use crate::log::HeadRef;
use crate::party::{PartyId, verify_party_key};
use crate::sig::ed25519::verify_ed25519_strict;
use crate::signable::AgentSignable;
use crate::state::ControlState;

/// Largest head claim, in bytes.
pub const MAX_HEAD_CLAIM_BYTES: usize = 1024;

/// The head of the agent's accepted log, as the agent states it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HeadClaim {
    pub network: Network,
    pub agent_party: PartyId,
    pub agent_key: [u8; 32],
    /// `None` for an empty log, written as seq 0 and a zero hash.
    pub head: Option<HeadRef>,
    pub issued_at: UtcTime,
}

impl HeadClaim {
    /// The claim for the head of `state`.
    pub fn for_state(state: &ControlState, issued_at: UtcTime) -> Self {
        Self {
            network: state.network(),
            agent_party: state.agent_party().clone(),
            agent_key: *state.agent_key(),
            head: state.head(),
            issued_at,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_HEAD, MAX_HEAD_CLAIM_BYTES);
        w.line("network", self.network.as_str());
        w.line("agent_party", self.agent_party.as_str());
        w.line("agent_key", &hex::encode(self.agent_key));
        let (seq, hash) = self
            .head
            .map_or((0, Hash32([0; 32])), |head| (head.seq, head.hash));
        w.line("seq", &seq.to_string());
        w.line("head", &hash.to_string());
        if self
            .head
            .is_some_and(|head| head.seq == 0 || head.hash == Hash32([0; 32]))
        {
            w.fail(CanonErrorKind::InvalidHead);
        }
        w.line("issued_at", &self.issued_at.to_string());
        w.finish()
    }

    /// Strict decoding: every rule, and the bytes must re-encode identically.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(
            bytes,
            SCHEME_HEAD,
            MAX_HEAD_CLAIM_BYTES,
            Self::read,
            Self::encode,
        )
    }

    /// The bytes the agent signs.
    pub fn signable(&self) -> Result<AgentSignable, CanonError> {
        self.encode().map(AgentSignable::head_claim)
    }

    /// Decodes and checks the network, the party, the key binding and the signature.
    pub fn verify(
        bytes: &[u8],
        signature: &[u8; 64],
        network: Network,
        expected_party: &PartyId,
    ) -> Result<Self, SignedDocError> {
        let claim = Self::decode(bytes).map_err(SignedDocError::Canon)?;
        if claim.network != network {
            return Err(SignedDocError::WrongNetwork);
        }
        if claim.agent_party != *expected_party {
            return Err(SignedDocError::WrongParty);
        }
        if !verify_party_key(&claim.agent_party, &claim.agent_key) {
            return Err(SignedDocError::KeyPartyMismatch);
        }
        verify_ed25519_strict(&claim.agent_key, bytes, signature)
            .map_err(SignedDocError::Signature)?;
        Ok(claim)
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, CanonError> {
        let network = r.value("network", Network::parse)?;
        let agent_party = r.value("agent_party", PartyId::parse)?;
        let agent_key = r.value("agent_key", |s| {
            parse_hex_array::<32>(s).ok_or(ValueError(ValueKind::Hex32))
        })?;
        let seq = r.value("seq", parse_u64)?;
        let hash = r.value("head", Hash32::parse)?;
        let zero = hash == Hash32([0; 32]);
        let head = match (seq, zero) {
            (0, true) => None,
            (seq, false) if seq >= 1 => Some(HeadRef { seq, hash }),
            _ => return Err(r.err(CanonErrorKind::InvalidHead)),
        };
        let issued_at = r.value("issued_at", UtcTime::parse)?;
        Ok(Self {
            network,
            agent_party,
            agent_key,
            head,
            issued_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signable::{SignablePurpose, sign_agent};
    use crate::testkit::{self, Chain, at};

    #[test]
    fn claims_round_trip_sign_and_verify() {
        let chain = Chain::linked();
        let claim = HeadClaim::for_state(&chain.state, at(30));
        assert_eq!(claim.head, chain.state.head());
        let empty = HeadClaim::for_state(&testkit::empty_state(), at(30));
        assert_eq!(empty.head, None);
        for claim in [claim, empty] {
            let signable = claim.signable().unwrap();
            assert_eq!(signable.purpose(), SignablePurpose::HeadClaim);
            let signature = sign_agent(&testkit::agent_key(), &signable);
            let party = testkit::agent_party();
            let verified =
                HeadClaim::verify(signable.bytes(), &signature, Network::Devnet, &party).unwrap();
            assert_eq!(verified, claim);
            let refusal = HeadClaim::verify(signable.bytes(), &signature, Network::Mainnet, &party)
                .unwrap_err();
            assert_eq!(refusal.code_name(), "wrong_network");
            let mut forged = signature;
            forged[63] ^= 0x10;
            let refusal =
                HeadClaim::verify(signable.bytes(), &forged, Network::Devnet, &party).unwrap_err();
            assert_eq!(refusal.code_name(), "bad_signature");
        }
    }

    #[test]
    fn the_head_and_seq_must_agree() {
        let chain = Chain::linked();
        let claim = HeadClaim::for_state(&chain.state, at(30));
        let text = String::from_utf8(claim.encode().unwrap()).unwrap();
        let hash = chain.state.head().unwrap().hash.to_string();
        let zeros = "0".repeat(64);
        let cases = [
            text.replacen("seq=1\n", "seq=0\n", 1),
            text.replacen(&hash, &zeros, 1),
        ];
        for case in cases {
            let err = HeadClaim::decode(case.as_bytes()).unwrap_err();
            assert_eq!(err.code_name(), "invalid_head");
        }
        let mut bad = claim.clone();
        bad.head = Some(HeadRef {
            seq: 0,
            hash: Hash32([1; 32]),
        });
        assert_eq!(bad.encode().unwrap_err().code_name(), "invalid_head");
        assert!(text.starts_with("scheme=silvana.agent.control-head.v1\nnetwork=devnet\n"));
        assert!(text.contains(&format!(
            "\nseq=1\nhead={hash}\nissued_at=2026-11-22T12:00:30Z\n"
        )));
    }
}
