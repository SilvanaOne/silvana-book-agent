//! Why a controller step did not succeed.

use std::fmt;

use agent_control::ChallengeError;

/// Every failure of the controller library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControllerError {
    /// The key file is missing, unreadable or refused.
    KeyFile(String),
    /// No answer: transport error, timeout, Unavailable or DeadlineExceeded.
    Unreachable(String),
    /// The rpc has no controller service.
    Unimplemented(String),
    /// Any other answer; `code` is N of `request refused (code N)`.
    Refused { code: Option<u16>, status: String },
    /// A sign-in challenge failed the local checks; nothing was signed.
    Challenge(ChallengeError),
    /// The local clock and the rpc's differ too much, or the rpc's time is unknown.
    Clock(String),
    /// The log the rpc served does not verify or does not replay.
    Log(String),
    /// The pinned head refuses: the rpc served a shorter or divergent log.
    Pin(String),
    /// A draft or a local check refused; nothing was signed or sent.
    Refusal(String),
    /// A local file could not be read or written.
    Io(String),
    /// A submission was sent, but its answer was lost or did not match it: the rpc may hold it.
    Unconfirmed(String),
}

impl ControllerError {
    /// N of `request refused (code N)`, when the rpc answered one.
    pub fn code(&self) -> Option<u16> {
        match self {
            Self::Refused { code, .. } => *code,
            _ => None,
        }
    }

    /// Maps a gRPC status.
    pub fn from_status(status: &tonic::Status) -> Self {
        use tonic::Code;
        let text = format!("{:?}: {}", status.code(), status.message());
        match status.code() {
            Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled => Self::Unreachable(text),
            Code::Unimplemented => Self::Unimplemented(text),
            _ => Self::Refused {
                code: refusal_code(status.message()),
                status: text,
            },
        }
    }

    /// The error of a submission already sent: only an opaque refusal or a missing service
    /// proves that the rpc did not store it.
    pub fn after_send(self) -> Self {
        match self {
            Self::Unreachable(text)
            | Self::Refused {
                code: None,
                status: text,
            } => Self::Unconfirmed(text),
            other => other,
        }
    }

    pub fn is_unconfirmed(&self) -> bool {
        matches!(self, Self::Unconfirmed(_))
    }
}

/// N of a `request refused (code N)` message.
pub fn refusal_code(message: &str) -> Option<u16> {
    let rest = message.split("request refused (code ").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if !rest.get(digits.len()..).is_some_and(|r| r.starts_with(')')) {
        return None;
    }
    digits.parse().ok()
}

impl fmt::Display for ControllerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyFile(text) => write!(f, "controller key: {text}"),
            Self::Unreachable(text) => write!(f, "the rpc is unreachable ({text})"),
            Self::Unimplemented(text) => write!(f, "the rpc has no controller service ({text})"),
            Self::Refused { status, .. } => write!(f, "the rpc refused the call ({status})"),
            Self::Challenge(e) => write!(
                f,
                "refusing to sign the sign-in challenge: {} (code {})",
                e.code_name(),
                e.code()
            ),
            Self::Clock(text) => write!(f, "refusing to sign: {text}"),
            Self::Log(text) => write!(f, "the agent's log does not verify: {text}"),
            Self::Pin(text) => write!(f, "refusing to sign: {text}"),
            Self::Refusal(text) => write!(f, "{text}"),
            Self::Io(text) => write!(f, "{text}"),
            Self::Unconfirmed(text) => {
                write!(
                    f,
                    "the rpc may hold the submission, its answer was lost ({text})"
                )
            }
        }
    }
}

impl std::error::Error for ControllerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_is_read_only_from_the_opaque_text() {
        assert_eq!(refusal_code("request refused (code 4096)"), Some(4096));
        assert_eq!(refusal_code("x: request refused (code 4097) y"), Some(4097));
        assert_eq!(refusal_code("request refused (code 40x)"), None);
        assert_eq!(refusal_code("refused"), None);
        let status = tonic::Status::permission_denied("request refused (code 4086)");
        assert_eq!(ControllerError::from_status(&status).code(), Some(4086));
        let status = tonic::Status::unavailable("down");
        assert!(matches!(
            ControllerError::from_status(&status),
            ControllerError::Unreachable(_)
        ));
    }

    #[test]
    fn only_an_opaque_refusal_proves_that_a_sent_submission_was_not_stored() {
        let of = |status: tonic::Status| ControllerError::from_status(&status).after_send();
        assert!(of(tonic::Status::unavailable("down")).is_unconfirmed());
        assert!(of(tonic::Status::deadline_exceeded("late")).is_unconfirmed());
        assert!(of(tonic::Status::internal("boom")).is_unconfirmed());
        assert!(
            ControllerError::Unreachable("no answer within 15s".into())
                .after_send()
                .is_unconfirmed()
        );
        let refused = of(tonic::Status::permission_denied(
            "request refused (code 4084)",
        ));
        assert_eq!(refused.code(), Some(4084));
        assert!(!of(tonic::Status::unimplemented("none")).is_unconfirmed());
        assert!(
            !ControllerError::Refusal("local".into())
                .after_send()
                .is_unconfirmed()
        );
    }
}
