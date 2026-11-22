//! Documents the agent key may sign. Only this crate builds them.

use ed25519_dalek::{Signer, SigningKey};

/// What an agent signature covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SignablePurpose {
    LogEntry,
    Receipt,
    HeadClaim,
}

impl SignablePurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LogEntry => "log_entry",
            Self::Receipt => "receipt",
            Self::HeadClaim => "head_claim",
        }
    }
}

/// Canonical bytes built by this crate, handed to the agent's signer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSignable {
    purpose: SignablePurpose,
    bytes: Vec<u8>,
}

impl AgentSignable {
    pub(crate) fn log_entry(bytes: Vec<u8>) -> Self {
        Self {
            purpose: SignablePurpose::LogEntry,
            bytes,
        }
    }

    pub(crate) fn receipt(bytes: Vec<u8>) -> Self {
        Self {
            purpose: SignablePurpose::Receipt,
            bytes,
        }
    }

    pub(crate) fn head_claim(bytes: Vec<u8>) -> Self {
        Self {
            purpose: SignablePurpose::HeadClaim,
            bytes,
        }
    }

    pub fn purpose(&self) -> SignablePurpose {
        self.purpose
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Ed25519 over the full bytes (RFC 8032 PureEdDSA), never pre-hashed.
pub fn sign_agent(key: &SigningKey, signable: &AgentSignable) -> [u8; 64] {
    key.sign(&signable.bytes).to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sig::ed25519::verify_ed25519_strict;
    use crate::testkit;

    #[test]
    fn signs_the_full_bytes() {
        let signable = AgentSignable::log_entry(b"scheme=silvana.agent.control.v1\n".to_vec());
        assert_eq!(signable.purpose(), SignablePurpose::LogEntry);
        assert_eq!(signable.purpose().as_str(), "log_entry");
        let key = testkit::agent_key();
        let signature = sign_agent(&key, &signable);
        let public = key.verifying_key().to_bytes();
        assert_eq!(
            verify_ed25519_strict(&public, signable.bytes(), &signature),
            Ok(())
        );
    }
}
