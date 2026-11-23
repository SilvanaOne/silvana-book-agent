//! Directed-transaction rules every enforcer shares: the Canton command id and the roots.

use std::fmt;

use crate::canon::values::{InstrumentId, InstrumentRef};
use crate::command::{CANTON_COMMAND_PREFIX, Command, CommandOp};
use crate::error::{CommandFailure, ValueError};
use crate::party::PartyId;

/// The choice of an atomic settle on a venue.
pub const SETTLE_CHOICE: &str = "AtomicDVP_Settle";
/// The venue template, as `module:entity`.
pub const VENUE_TEMPLATE: &str = "AtomicDVP:AtomicDVP";
/// The root choice of a CC or CIP-56 transfer.
pub const TRANSFER_CHOICE: &str = "TransferFactory_Transfer";
/// The instrument id of Canton Coin holdings, whose admin is the DSO party.
pub const CC_INSTRUMENT_ID: &str = "Amulet";

/// Why a transaction is not the one a command directs, or carries a directed id without
/// one. It has no code of its own: a receipt reports it as `content_refused`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindingError {
    /// A directed transaction does not carry exactly the command's Canton command id.
    CommandIdMismatch,
    /// An undirected transaction carries the directed prefix.
    ReservedCommandId,
    /// The transaction does not have exactly one root node.
    RootCount,
    /// The one root node is not an exercise.
    RootNotExercise,
    /// The root exercise is not the one the operation runs.
    UnexpectedRoot,
    /// A swap was bound without the venue the caller requested.
    NoVenue,
    /// The settle runs on another venue.
    WrongVenue,
    /// The operation is not executed by directed commands.
    NotExecuted,
    /// The command directs another operation than the one bound.
    WrongOperation,
    /// The evaluation context names another agent than the control state.
    WrongAgent,
    /// The transaction is not submitted by the agent alone.
    ActAs,
    /// The transaction creates or exercises templates outside the model.
    Unmodelled,
    /// The transaction creates or reads DvP terms, which no directed command runs.
    UnexpectedDvp,
    /// The transaction runs no atomic settle.
    NoSettle,
    /// The settle's venue is not among the input contracts.
    NoVenueTerms,
    /// The settle's user is not the agent.
    WrongUser,
    /// A user input of the settle is not a consumed holding of the agent.
    ForeignInput,
    /// A consumed contract is neither created in the transaction nor listed as an input.
    UnlistedInput,
    /// A holding created for the agent is locked.
    LockedInflow,
    /// The agent ends with less of the received instrument than it consumed.
    NegativeNet,
    /// An amount the amount grammar cannot hold exactly.
    InexactAmount,
    /// A sum of amounts leaves the amount grammar.
    AmountOverflow,
    /// A party the terms need does not parse.
    BadParty,
    /// An instrument does not parse.
    BadInstrument,
    /// The venue's pair name is not a market id.
    BadMarket,
    /// The quote's expiry is out of range.
    BadExpiry,
    /// The content is not exactly the command's operation.
    ContentMismatch,
}

impl BindingError {
    /// Stable name of the refusal.
    pub fn code_name(self) -> &'static str {
        match self {
            Self::CommandIdMismatch => "command_id_mismatch",
            Self::ReservedCommandId => "reserved_command_id",
            Self::RootCount => "root_count",
            Self::RootNotExercise => "root_not_exercise",
            Self::UnexpectedRoot => "unexpected_root",
            Self::NoVenue => "no_venue",
            Self::WrongVenue => "wrong_venue",
            Self::NotExecuted => "not_executed",
            Self::WrongOperation => "wrong_operation",
            Self::WrongAgent => "wrong_agent",
            Self::ActAs => "act_as",
            Self::Unmodelled => "unmodelled",
            Self::UnexpectedDvp => "unexpected_dvp",
            Self::NoSettle => "no_settle",
            Self::NoVenueTerms => "no_venue_terms",
            Self::WrongUser => "wrong_user",
            Self::ForeignInput => "foreign_input",
            Self::UnlistedInput => "unlisted_input",
            Self::LockedInflow => "locked_inflow",
            Self::NegativeNet => "negative_net",
            Self::InexactAmount => "inexact_amount",
            Self::AmountOverflow => "amount_overflow",
            Self::BadParty => "bad_party",
            Self::BadInstrument => "bad_instrument",
            Self::BadMarket => "bad_market",
            Self::BadExpiry => "bad_expiry",
            Self::ContentMismatch => "content_mismatch",
        }
    }

    /// The failure an agent receipt reports for any binding refusal.
    pub fn failure(self) -> CommandFailure {
        CommandFailure::ContentRefused
    }
}

impl fmt::Display for BindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code_name())
    }
}

impl std::error::Error for BindingError {}

/// A directed transaction carries exactly the command's Canton command id; any other
/// carries an id that does not start with the directed prefix (bytes, case-sensitive).
pub fn check_command_id(
    directed: Option<&Command>,
    tx_command_id: &str,
) -> Result<(), BindingError> {
    match directed {
        Some(command) if tx_command_id == command.canton_command_id() => Ok(()),
        Some(_) => Err(BindingError::CommandIdMismatch),
        None if tx_command_id.starts_with(CANTON_COMMAND_PREFIX) => {
            Err(BindingError::ReservedCommandId)
        }
        None => Ok(()),
    }
}

/// One root exercise: the template as `module:entity`, the choice and the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RootExercise<'a> {
    pub template: &'a str,
    pub choice: &'a str,
    pub contract_id: &'a str,
}

/// A directed transaction has one root node, the exercise its operation runs: a swap settles
/// on the venue the caller requested, a transfer runs the transfer factory.
pub fn check_directed_roots(
    op: &CommandOp,
    roots: &[RootExercise<'_>],
    root_nodes: usize,
    venue_cid: Option<&str>,
) -> Result<(), BindingError> {
    if let CommandOp::UsdcxBurn { .. } = op {
        return Err(BindingError::NotExecuted);
    }
    if root_nodes != 1 || roots.len() > 1 {
        return Err(BindingError::RootCount);
    }
    let [root] = roots else {
        return Err(BindingError::RootNotExercise);
    };
    match op {
        CommandOp::Swap { .. } => {
            if root.template != VENUE_TEMPLATE || root.choice != SETTLE_CHOICE {
                return Err(BindingError::UnexpectedRoot);
            }
            let venue = venue_cid
                .filter(|cid| !cid.is_empty())
                .ok_or(BindingError::NoVenue)?;
            if root.contract_id == venue {
                Ok(())
            } else {
                Err(BindingError::WrongVenue)
            }
        }
        CommandOp::TransferCc { .. } | CommandOp::TransferToken { .. } => {
            if root.choice == TRANSFER_CHOICE {
                Ok(())
            } else {
                Err(BindingError::UnexpectedRoot)
            }
        }
        CommandOp::UsdcxBurn { .. } => Err(BindingError::NotExecuted),
    }
}

/// Canton Coin as transaction facts name it: the DSO party's `Amulet`. Every enforcer's
/// `EvalContext::cc` must equal it.
pub fn canton_coin(dso: &PartyId) -> Result<InstrumentRef, ValueError> {
    InstrumentId::parse(CC_INSTRUMENT_ID).map(|id| InstrumentRef {
        admin: dso.clone(),
        id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::time::UtcTime;
    use crate::canon::values::{Amount, EthAddress, Hash32, Network, Text, Uuid7};

    fn party(hint: &str, key: u8) -> PartyId {
        PartyId::parse(&format!("{hint}::1220{}", hex::encode([key; 32]))).unwrap()
    }

    fn amount(s: &str) -> Amount {
        Amount::parse(s).unwrap()
    }

    fn command(op: CommandOp) -> Command {
        let at = UtcTime::from_unix_secs(1_795_000_000).unwrap();
        Command {
            network: Network::Devnet,
            agent_party: party("agent", 1),
            command_id: Uuid7::parse("01890a5d-ac96-774b-bcce-b302099a8057").unwrap(),
            log_seq: 1,
            log_head: Hash32([1; 32]),
            issued_at: at,
            expires_at: at.checked_add_secs(600).unwrap(),
            op,
            summary: Text::parse("Swap").unwrap(),
        }
    }

    fn cc() -> InstrumentRef {
        canton_coin(&party("dso", 4)).unwrap()
    }

    fn swap() -> CommandOp {
        CommandOp::Swap {
            sell: cc(),
            sell_amount: amount("1000"),
            buy: InstrumentRef {
                admin: party("reg", 5),
                id: InstrumentId::parse("USDCx").unwrap(),
            },
            min_receive: amount("150"),
            max_fee_cc: amount("2"),
        }
    }

    fn pay() -> CommandOp {
        CommandOp::TransferCc {
            receiver: party("alice", 2),
            amount: amount("10"),
            description: None,
            settlement_proposal_id: None,
        }
    }

    fn root<'a>(template: &'a str, choice: &'a str, contract_id: &'a str) -> RootExercise<'a> {
        RootExercise {
            template,
            choice,
            contract_id,
        }
    }

    #[test]
    fn a_directed_id_is_exact_and_an_undirected_one_never_carries_the_prefix() {
        let directed = command(pay());
        let id = directed.canton_command_id();
        assert_eq!(id, "ctl-01890a5d-ac96-774b-bcce-b302099a8057");
        assert_eq!(check_command_id(Some(&directed), &id), Ok(()));
        for other in [
            "",
            "01890a5d-ac96-774b-bcce-b302099a8057",
            "CTL-01890a5d-ac96-774b-bcce-b302099a8057",
            "ctl-01890a5d-ac96-774b-bcce-b302099a8057 ",
            "ctl-01890a5d-ac96-774b-bcce-b302099a8058",
            "atomic-swap-1",
        ] {
            assert_eq!(
                check_command_id(Some(&directed), other),
                Err(BindingError::CommandIdMismatch),
                "{other:?}"
            );
        }
        for reserved in [id.as_str(), "ctl-", "ctl-anything"] {
            assert_eq!(
                check_command_id(None, reserved),
                Err(BindingError::ReservedCommandId),
                "{reserved:?}"
            );
        }
        for free in ["", "atomic-swap-1", "CTL-x", "ctl", "xctl-1", " ctl-1"] {
            assert_eq!(check_command_id(None, free), Ok(()), "{free:?}");
        }
    }

    #[test]
    fn a_swap_has_one_settle_root_on_the_requested_venue() {
        let op = swap();
        let settle = root(VENUE_TEMPLATE, SETTLE_CHOICE, "venue-1");
        assert_eq!(
            check_directed_roots(&op, &[settle], 1, Some("venue-1")),
            Ok(())
        );
        let refused = [
            // a second root, of any type
            (
                vec![settle, settle],
                2,
                Some("venue-1"),
                BindingError::RootCount,
            ),
            (vec![settle], 2, Some("venue-1"), BindingError::RootCount),
            (vec![], 0, Some("venue-1"), BindingError::RootCount),
            // the one root is a Create or a Fetch
            (vec![], 1, Some("venue-1"), BindingError::RootNotExercise),
            (
                vec![root(VENUE_TEMPLATE, "AtomicDVP_Retire", "venue-1")],
                1,
                Some("venue-1"),
                BindingError::UnexpectedRoot,
            ),
            (
                vec![root("AtomicDVP:SettlementTicket", SETTLE_CHOICE, "venue-1")],
                1,
                Some("venue-1"),
                BindingError::UnexpectedRoot,
            ),
            (
                vec![root(VENUE_TEMPLATE, TRANSFER_CHOICE, "venue-1")],
                1,
                Some("venue-1"),
                BindingError::UnexpectedRoot,
            ),
            (vec![settle], 1, Some("venue-2"), BindingError::WrongVenue),
            (vec![settle], 1, None, BindingError::NoVenue),
            (vec![settle], 1, Some(""), BindingError::NoVenue),
        ];
        for (roots, nodes, venue, error) in refused {
            assert_eq!(
                check_directed_roots(&op, &roots, nodes, venue),
                Err(error),
                "{roots:?} {nodes} {venue:?}"
            );
        }
    }

    #[test]
    fn a_transfer_has_one_transfer_factory_root_and_a_burn_is_not_executed() {
        let factory = root(
            "Splice.ExternalPartyAmuletRules:ExternalPartyAmuletRules",
            TRANSFER_CHOICE,
            "rules-1",
        );
        let token = CommandOp::TransferToken {
            receiver: party("alice", 2),
            instrument: cc(),
            amount: amount("10"),
            reference: None,
        };
        for op in [pay(), token] {
            assert_eq!(check_directed_roots(&op, &[factory], 1, None), Ok(()));
            assert_eq!(
                check_directed_roots(&op, &[factory, factory], 2, None),
                Err(BindingError::RootCount)
            );
            assert_eq!(
                check_directed_roots(&op, &[factory], 2, None),
                Err(BindingError::RootCount)
            );
            assert_eq!(
                check_directed_roots(&op, &[], 1, None),
                Err(BindingError::RootNotExercise)
            );
            let settle = root(VENUE_TEMPLATE, SETTLE_CHOICE, "venue-1");
            assert_eq!(
                check_directed_roots(&op, &[settle], 1, Some("venue-1")),
                Err(BindingError::UnexpectedRoot)
            );
        }
        let burn = CommandOp::UsdcxBurn {
            amount: amount("1"),
            destination: EthAddress([1; 20]),
        };
        assert_eq!(
            check_directed_roots(&burn, &[factory], 1, None),
            Err(BindingError::NotExecuted)
        );
    }

    #[test]
    fn binding_refusals_have_stable_names_and_report_content_refused() {
        assert_eq!(
            BindingError::CommandIdMismatch.code_name(),
            "command_id_mismatch"
        );
        assert_eq!(
            BindingError::ContentMismatch.to_string(),
            "content_mismatch"
        );
        assert_eq!(BindingError::UnexpectedDvp.code_name(), "unexpected_dvp");
        assert_eq!(BindingError::RootCount.failure().code(), 1407);
        assert_eq!(BindingError::UnexpectedDvp.failure().code(), 1407);
        assert_eq!(
            cc(),
            InstrumentRef {
                admin: party("dso", 4),
                id: InstrumentId::parse("Amulet").unwrap(),
            }
        );
    }
}
