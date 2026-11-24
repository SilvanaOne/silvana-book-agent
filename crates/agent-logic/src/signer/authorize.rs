//! Consent for signatures that spend.

use std::fmt;

/// One spend a signature needs consent for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spend<'a> {
    /// The prepared transaction `transaction_id`.
    Transaction { transaction_id: &'a str },
    /// The fee schedule `fees_json`, under a transaction or pay-fee session id.
    FeeConsent { key: &'a str, fees_json: &'a str },
    /// The RFQ V2 quote `quote_id`.
    Quote { quote_id: &'a str },
}

/// The guard's consent to a set of spends; spending signatures need one.
#[derive(Debug)]
#[must_use]
pub struct Authorization {
    guard_id: u64,
    scope: Scope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// No policy limits the spends.
    Unrestricted,
}

impl Authorization {
    pub(crate) fn unrestricted(guard_id: u64) -> Self {
        Self {
            guard_id,
            scope: Scope::Unrestricted,
        }
    }

    /// True when no policy limits the spends.
    pub fn is_unrestricted(&self) -> bool {
        self.scope == Scope::Unrestricted
    }

    /// True when the guard `guard_id` issued it.
    pub(crate) fn issued_by(&self, guard_id: u64) -> bool {
        self.guard_id == guard_id
    }
}

/// A refused authorization. Only the code is displayed; the detail is for logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    code: u16,
    detail: String,
}

impl Refusal {
    pub fn new(code: u16, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    pub fn code(&self) -> u16 {
        self.code
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "request refused (code {})", self.code)
    }
}

impl std::error::Error for Refusal {}
