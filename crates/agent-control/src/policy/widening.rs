//! Which categories a new policy widens, and the delay before it takes effect.

use super::scopes::scopes_widen;
use super::{Allowlist, CapValue, Limits, PolicyDocument, Rules};
use crate::canon::time::UtcTime;
use crate::canon::values::{Amount, Network};
use crate::state::{add_secs, controller_change_delay_secs};

/// The categories in which a policy grants more than its baseline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Widening {
    pub transfers: bool,
    pub trading: bool,
    pub other: bool,
    /// A package id added, or a selection opened to any package.
    pub packages: bool,
    /// A direct controller may sign more commands, or swaps on looser terms.
    pub scopes: bool,
}

impl Widening {
    pub fn any(&self) -> bool {
        self.transfers || self.trading || self.other || self.packages || self.scopes
    }
}

fn switched_on(old: bool, new: bool) -> bool {
    !old && new
}

fn gains_member<T: PartialEq>(old: &Allowlist<T>, new: &Allowlist<T>) -> bool {
    if new.any {
        return !old.any;
    }
    !old.any && new.items.iter().any(|item| !old.items.contains(item))
}

fn cap_rises(old: Option<Amount>, new: Option<Amount>) -> bool {
    CapValue::of_optional(new) > CapValue::of_optional(old)
}

/// A rise of the unlisted cap, or of any instrument named in either list.
fn limits_rise(old: &Limits, new: &Limits) -> bool {
    new.unlisted_cap() > old.unlisted_cap()
        || old
            .items
            .iter()
            .chain(new.items.iter())
            .any(|item| new.cap_for(&item.instrument) > old.cap_for(&item.instrument))
}

pub fn widening(old: &Rules, new: &Rules) -> Widening {
    let (ot, nt) = (&old.transfers, &new.transfers);
    let transfers = switched_on(ot.enabled, nt.enabled)
        || (ot.directed_only && !nt.directed_only)
        || gains_member(&ot.recipients, &nt.recipients)
        || gains_member(&ot.eth_destinations, &nt.eth_destinations)
        || limits_rise(&ot.per_transfer, &nt.per_transfer)
        || limits_rise(&ot.per_day, &nt.per_day)
        || new.delays.transfers_secs < old.delays.transfers_secs;
    let (og, ng) = (&old.trading, &new.trading);
    let trading = switched_on(og.enabled, ng.enabled)
        || switched_on(og.lp_enabled, ng.lp_enabled)
        || gains_member(&og.markets, &ng.markets)
        || gains_member(&og.instruments, &ng.instruments)
        || limits_rise(&og.per_trade, &ng.per_trade)
        || limits_rise(&og.per_day, &ng.per_day)
        || new.delays.trading_secs < old.delays.trading_secs;
    let other = (old.paused && !new.paused)
        || switched_on(old.maintenance.enabled, new.maintenance.enabled)
        || switched_on(old.locks.enabled, new.locks.enabled)
        || switched_on(old.usdcx.enabled, new.usdcx.enabled)
        || cap_rises(
            old.maintenance.traffic_cc_per_day,
            new.maintenance.traffic_cc_per_day,
        )
        || limits_rise(&old.locks.per_lock, &new.locks.per_lock)
        || cap_rises(old.fees.cc_per_consent, new.fees.cc_per_consent)
        || cap_rises(old.fees.cc_per_day, new.fees.cc_per_day)
        || new.delays.other_secs < old.delays.other_secs;
    Widening {
        transfers,
        trading,
        other,
        packages: gains_member(&old.packages, &new.packages),
        scopes: scopes_widen(&old.scopes, &new.scopes),
    }
}

/// The largest delay among the categories `new` widens, or 0. Packages and scopes wait the
/// network's controller-change delay, which the policy's own delays cannot shorten.
pub fn required_delay_secs(network: Network, baseline: &Rules, new: &Rules) -> u64 {
    let widened = widening(baseline, new);
    let delays = &baseline.delays;
    let controller_delay = controller_change_delay_secs(network);
    [
        (widened.transfers, delays.transfers_secs),
        (widened.trading, delays.trading_secs),
        (widened.other, delays.other_secs),
        (widened.packages, controller_delay),
        (widened.scopes, controller_delay),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, secs)| secs)
    .max()
    .unwrap_or(0)
}

/// max(accepted_at + the required delay on the document's network, not_before);
/// `None` past the time range.
pub fn expected_effective_at(
    accepted_at: UtcTime,
    baseline: &Rules,
    doc: &PolicyDocument,
) -> Option<UtcTime> {
    let delayed = add_secs(
        accepted_at,
        required_delay_secs(doc.network, baseline, &doc.rules),
    )?;
    Some(match doc.not_before {
        Some(not_before) if not_before > delayed => not_before,
        _ => delayed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::values::{Network, PackageId};
    use crate::command::CommandKind;
    use crate::entry::{LogEntry, SignedEntry};
    use crate::log::verify_log;
    use crate::policy::scopes::CommandScope;
    use crate::policy::{DEFAULT_LINKED_RULES, LimitItem, PolicyInstrument, Unlisted};
    use crate::state::replay;
    use crate::testkit::{self, Chain, at};
    use ed25519_dalek::Signer;

    fn amount(s: &str) -> Amount {
        Amount::parse(s).unwrap()
    }

    fn cc_limit(value: &str) -> Limits {
        Limits {
            unlisted: Unlisted::Unlimited,
            items: vec![LimitItem {
                instrument: PolicyInstrument::Cc,
                amount: amount(value),
            }],
        }
    }

    fn check(old: &Rules, new: &Rules) -> (bool, bool, bool) {
        let w = widening(old, new);
        (w.transfers, w.trading, w.other)
    }

    #[test]
    fn narrowing_widens_nothing() {
        let base = DEFAULT_LINKED_RULES;
        let mut narrow = base.clone();
        narrow.paused = true;
        narrow.transfers.enabled = false;
        narrow.trading.lp_enabled = false;
        narrow.trading.per_trade = cc_limit("10");
        narrow.fees.cc_per_day = Some(amount("1"));
        narrow.delays.transfers_secs = 172_800;
        assert!(!widening(&base, &narrow).any());
        assert_eq!(required_delay_secs(Network::Devnet, &base, &narrow), 0);
        assert_eq!(check(&narrow, &base), (true, true, true));
    }

    #[test]
    fn each_category_has_its_own_triggers() {
        let base = DEFAULT_LINKED_RULES;
        let mut undirected = base.clone();
        undirected.transfers.directed_only = false;
        assert_eq!(check(&base, &undirected), (true, false, false));
        assert_eq!(
            required_delay_secs(Network::Devnet, &base, &undirected),
            86_400
        );
        let mut faster = base.clone();
        faster.delays.transfers_secs = 3600;
        assert_eq!(check(&base, &faster), (true, false, false));
        assert_eq!(required_delay_secs(Network::Devnet, &base, &faster), 86_400);
        let mut capped = base.clone();
        capped.trading.per_day = cc_limit("10");
        let mut raised = capped.clone();
        raised.trading.per_day = cc_limit("10.5");
        assert_eq!(check(&capped, &raised), (false, true, false));
        assert_eq!(required_delay_secs(Network::Devnet, &capped, &raised), 0);
        let mut paused = base.clone();
        paused.paused = true;
        assert_eq!(check(&paused, &base), (false, false, true));
        let changes: [fn(&mut Rules); 6] = [
            |r: &mut Rules| r.locks.enabled = false,
            |r: &mut Rules| r.usdcx.enabled = false,
            |r: &mut Rules| r.maintenance.enabled = false,
            |r: &mut Rules| r.fees.cc_per_consent = Some(Amount::ZERO),
            |r: &mut Rules| r.maintenance.traffic_cc_per_day = Some(Amount::ZERO),
            |r: &mut Rules| r.locks.per_lock = cc_limit("1"),
        ];
        for change in changes {
            let mut old = base.clone();
            change(&mut old);
            assert_eq!(check(&old, &base), (false, false, true));
        }
        let mut slow_other = base.clone();
        slow_other.delays.other_secs = 600;
        let mut unpaused = slow_other.clone();
        unpaused.paused = true;
        assert_eq!(
            required_delay_secs(Network::Devnet, &unpaused, &slow_other),
            600
        );
    }

    #[test]
    fn allowlists_widen_when_they_gain_a_member() {
        let base = DEFAULT_LINKED_RULES;
        let alice = testkit::agent_party();
        let mut listed = base.clone();
        listed.transfers.recipients = Allowlist {
            any: false,
            items: vec![alice.clone()],
        };
        assert_eq!(check(&listed, &base), (true, false, false));
        assert_eq!(check(&base, &listed), (false, false, false));
        let mut empty = listed.clone();
        empty.transfers.recipients.items.clear();
        assert_eq!(check(&empty, &listed), (true, false, false));
        assert_eq!(check(&listed, &empty), (false, false, false));
        let mut markets = base.clone();
        markets.trading.markets = Allowlist {
            any: false,
            items: Vec::new(),
        };
        assert_eq!(check(&markets, &base), (false, true, false));
    }

    #[test]
    fn caps_order_denied_below_amounts_below_unlimited() {
        assert!(CapValue::Denied < CapValue::Amount(Amount::ZERO));
        assert!(CapValue::Amount(amount("5")) < CapValue::Amount(amount("10")));
        assert!(CapValue::Amount(amount("99999999999999999")) < CapValue::Unlimited);
        let deny = Limits {
            unlisted: Unlisted::Deny,
            items: Vec::new(),
        };
        let deny_listed = Limits {
            unlisted: Unlisted::Deny,
            items: cc_limit("1").items,
        };
        let mut old = DEFAULT_LINKED_RULES;
        old.transfers.per_transfer = deny.clone();
        let mut new = old.clone();
        new.transfers.per_transfer = deny_listed.clone();
        assert_eq!(check(&old, &new), (true, false, false));
        assert_eq!(check(&new, &old), (false, false, false));
        // Dropping a listed cap falls back to the unlisted one.
        let mut unlimited_listed = old.clone();
        unlimited_listed.transfers.per_transfer = Limits {
            unlisted: Unlisted::Unlimited,
            items: cc_limit("1").items,
        };
        let mut unlimited = old.clone();
        unlimited.transfers.per_transfer = Limits::UNLIMITED;
        assert_eq!(check(&unlimited_listed, &unlimited), (true, false, false));
    }

    #[test]
    fn not_before_later_than_the_delay_wins() {
        let accepted = testkit::t("2026-11-22T12:00:00Z");
        let mut doc = crate::policy::PolicyDocument {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            version: 1,
            prev_policy_hash: crate::canon::values::Hash32([0; 32]),
            issued_at: accepted,
            expires_at: testkit::t("2026-11-22T13:00:00Z"),
            not_before: None,
            rules: DEFAULT_LINKED_RULES,
        };
        let base = DEFAULT_LINKED_RULES;
        assert_eq!(expected_effective_at(accepted, &base, &doc), Some(accepted));
        doc.not_before = Some(testkit::t("2026-11-24T00:00:00Z"));
        assert_eq!(expected_effective_at(accepted, &base, &doc), doc.not_before);
        doc.rules.transfers.directed_only = false;
        doc.not_before = Some(testkit::t("2026-11-23T00:00:00Z"));
        assert_eq!(
            expected_effective_at(accepted, &base, &doc),
            Some(testkit::t("2026-11-23T12:00:00Z"))
        );
        let end = testkit::t("9999-12-31T00:00:00Z");
        assert_eq!(expected_effective_at(end, &base, &doc), None);
    }

    fn selected(ids: &[u8]) -> Rules {
        let mut rules = DEFAULT_LINKED_RULES;
        rules.packages = Allowlist {
            any: false,
            items: ids.iter().map(|b| PackageId([*b; 32])).collect(),
        };
        rules
    }

    fn scoped(kinds: &[CommandKind]) -> Rules {
        let mut rules = DEFAULT_LINKED_RULES;
        rules.scopes = vec![CommandScope {
            controller: testkit::ed25519_id(0x21),
            commands: kinds.to_vec(),
            pairs: Vec::new(),
            lps: Vec::new(),
        }];
        rules
    }

    const NETWORKS: [(Network, u64); 2] = [(Network::Devnet, 3_600), (Network::Mainnet, 86_400)];

    #[test]
    fn packages_widen_when_they_gain_an_id_or_open_to_any() {
        let any = DEFAULT_LINKED_RULES;
        let (one, two) = (selected(&[1]), selected(&[1, 2]));
        let other = selected(&[2]);
        for (old, new, widens) in [
            (&any, &one, false),
            (&two, &one, false),
            (&one, &one, false),
            (&one, &two, true),
            (&one, &other, true),
            (&one, &any, true),
        ] {
            let w = widening(old, new);
            assert_eq!(w.packages, widens, "{old:?} -> {new:?}");
            assert_eq!(w.any(), widens);
            assert_eq!(
                (w.transfers, w.trading, w.other, w.scopes),
                (false, false, false, false)
            );
            for (network, delay) in NETWORKS {
                let expected = if widens { delay } else { 0 };
                assert_eq!(required_delay_secs(network, old, new), expected);
            }
        }
    }

    #[test]
    fn scopes_widen_after_the_controller_delay_of_the_network() {
        let unscoped = DEFAULT_LINKED_RULES;
        let swap = scoped(&[CommandKind::Swap]);
        let none = scoped(&[]);
        for (old, new, widens) in [
            (&unscoped, &swap, false),
            (&swap, &none, false),
            (&unscoped, &none, false),
            (&swap, &unscoped, true),
            (&none, &swap, true),
            (
                &swap,
                &scoped(&[CommandKind::Swap, CommandKind::TransferCc]),
                true,
            ),
        ] {
            let w = widening(old, new);
            assert_eq!((w.scopes, w.any()), (widens, widens));
            for (network, delay) in NETWORKS {
                let expected = if widens { delay } else { 0 };
                assert_eq!(required_delay_secs(network, old, new), expected);
            }
        }
    }

    #[test]
    fn the_policy_delays_never_shorten_a_package_or_scope_widening() {
        let mut fast = scoped(&[CommandKind::Swap]);
        fast.delays = crate::policy::Delays {
            transfers_secs: 0,
            trading_secs: 0,
            other_secs: 0,
        };
        let mut open = fast.clone();
        open.scopes.clear();
        open.packages = Allowlist::ANY;
        let mut narrow = fast.clone();
        narrow.packages = Allowlist {
            any: false,
            items: vec![PackageId([7; 32])],
        };
        for (network, delay) in NETWORKS {
            assert_eq!(required_delay_secs(network, &fast, &open), delay);
            assert_eq!(required_delay_secs(network, &narrow, &fast), delay);
        }
        // A longer delay of another widened category still wins.
        let mut slow = fast.clone();
        slow.delays.transfers_secs = 172_800;
        let mut both = slow.clone();
        both.scopes.clear();
        both.transfers.directed_only = false;
        for (network, _) in NETWORKS {
            assert_eq!(required_delay_secs(network, &slow, &both), 172_800);
        }
        let mut short = fast.clone();
        short.delays.transfers_secs = 600;
        let mut wider = short.clone();
        wider.scopes.clear();
        wider.transfers.directed_only = false;
        assert_eq!(required_delay_secs(Network::Devnet, &short, &wider), 3_600);
        assert_eq!(
            required_delay_secs(Network::Mainnet, &short, &wider),
            86_400
        );
    }

    #[test]
    fn the_effective_time_follows_the_document_network() {
        let accepted = testkit::t("2026-11-22T12:00:00Z");
        let baseline = scoped(&[CommandKind::Swap]);
        let mut doc = crate::policy::PolicyDocument {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            version: 2,
            prev_policy_hash: crate::canon::values::Hash32([3; 32]),
            issued_at: accepted,
            expires_at: testkit::t("2026-11-22T13:00:00Z"),
            not_before: None,
            rules: DEFAULT_LINKED_RULES,
        };
        assert_eq!(
            expected_effective_at(accepted, &baseline, &doc),
            Some(testkit::t("2026-11-22T13:00:00Z"))
        );
        doc.network = Network::Mainnet;
        assert_eq!(
            expected_effective_at(accepted, &baseline, &doc),
            Some(testkit::t("2026-11-23T12:00:00Z"))
        );
        doc.not_before = Some(testkit::t("2026-11-24T00:00:00Z"));
        assert_eq!(
            expected_effective_at(accepted, &baseline, &doc),
            doc.not_before
        );
        doc.not_before = None;
        doc.rules = scoped(&[]);
        assert_eq!(
            expected_effective_at(accepted, &baseline, &doc),
            Some(accepted)
        );
    }

    /// `entry` signed again with another effective time.
    fn retimed(entry: &LogEntry, effective_at: UtcTime) -> SignedEntry {
        let mut entry = entry.clone();
        entry.effective_at = effective_at;
        let bytes = entry.encode().unwrap();
        let signature = testkit::agent_key().sign(&bytes).to_bytes();
        SignedEntry::from_parts(entry, bytes, signature)
    }

    fn doc_on(chain: &Chain, version: u64, when: UtcTime, rules: Rules) -> PolicyDocument {
        PolicyDocument {
            network: chain.state.network(),
            ..testkit::policy_doc(&chain.state, version, when, rules)
        }
    }

    #[test]
    fn replay_recomputes_scope_and_package_widening_delays_per_network() {
        let mismatch = Some("effective_at_mismatch");
        for (network, delay) in NETWORKS {
            let delay = i64::try_from(delay).unwrap();
            let mut chain = Chain::linked_on(network);
            // In effect once the link delay passed; a narrowing then applies at once.
            let t1 = at(delay + 10);
            let mut narrow = scoped(&[CommandKind::Swap]);
            narrow.packages = selected(&[1]).packages;
            let v1 = doc_on(&chain, 1, t1, narrow.clone());
            assert_eq!(chain.accept_ed25519(0x21, &v1, t1).effective_at(), t1);
            assert_eq!(chain.state.rules_at(t1).rules(), &narrow);
            // Dropping the scope and going back to any package both widen.
            let t2 = at(delay + 20);
            let v2 = doc_on(&chain, 2, t2, DEFAULT_LINKED_RULES);
            let bytes = v2.encode().unwrap();
            let envelope = testkit::ed25519_envelope(0x21, &bytes);
            let pending = chain
                .builder()
                .accept_policy(&bytes, &envelope, t2)
                .unwrap();
            let due = t2.checked_add_secs(delay).unwrap();
            assert_eq!(pending.entry().effective_at(), due);
            let mut docs = chain.docs.clone();
            docs.insert(bytes.clone());
            let wrong: &[(Network, i64)] = &[
                (Network::Mainnet, 3_600),
                (Network::Mainnet, 86_399),
                (Network::Mainnet, 0),
                (Network::Devnet, 86_400),
                (Network::Devnet, 3_599),
                (Network::Devnet, 0),
            ];
            for (on, secs) in wrong.iter().filter(|(on, _)| *on == network) {
                let entry = retimed(pending.entry(), t2.checked_add_secs(*secs).unwrap());
                let mut entries = chain.entries.clone();
                entries.push(entry);
                let log = verify_log(*on, &testkit::agent_party(), &entries).unwrap();
                let verdict = replay(&log, &docs).err().map(|e| e.code_name());
                assert_eq!(verdict, mismatch, "{on} {secs}");
            }
            chain.push(&pending);
            assert_eq!(chain.state.scheduled_policy(t2).unwrap().version(), 2);
            let before_due = due.checked_add_secs(-1).unwrap();
            assert_eq!(
                chain.state.rules_at(before_due).rules().scopes,
                v1.rules.scopes
            );
            assert!(chain.state.rules_at(due).rules().scopes.is_empty());
            assert_eq!(chain.replayed(), chain.state);
            // A narrowing during the delay applies at once and cancels the widening.
            let t3 = at(delay + 30);
            let mut stop = narrow.clone();
            stop.scopes = scoped(&[]).scopes;
            let v3 = doc_on(&chain, 3, t3, stop);
            assert_eq!(chain.accept_ed25519(0x21, &v3, t3).effective_at(), t3);
            assert!(chain.state.scheduled_policy(t3).is_none());
            for when in [due, at(30 * 86_400)] {
                let in_force = chain.state.in_force_policy(when).unwrap();
                assert_eq!(in_force.version(), 3);
                assert_eq!(in_force.rules().scopes, v3.rules.scopes);
            }
            assert_eq!(chain.replayed(), chain.state);
        }
    }

    #[test]
    fn zero_policy_delays_cannot_shorten_a_scope_widening_in_replay() {
        for (network, delay) in NETWORKS {
            let delay = i64::try_from(delay).unwrap();
            let mut chain = Chain::linked_on(network);
            let mut fast = scoped(&[CommandKind::Swap]);
            fast.delays = crate::policy::Delays {
                transfers_secs: 0,
                trading_secs: 0,
                other_secs: 0,
            };
            // Lowering the transfers delay waits the 24 h it had.
            let t1 = at(delay + 10);
            let v1 = doc_on(&chain, 1, t1, fast.clone());
            let in_force = chain.accept_ed25519(0x21, &v1, t1).effective_at();
            assert_eq!(in_force, t1.checked_add_secs(86_400).unwrap());
            let t2 = in_force.checked_add_secs(60).unwrap();
            assert_eq!(chain.state.rules_at(t2).rules().delays.transfers_secs, 0);
            let mut open = fast;
            open.scopes.clear();
            let v2 = doc_on(&chain, 2, t2, open);
            let effective = chain.accept_ed25519(0x21, &v2, t2).effective_at();
            assert_eq!(effective, t2.checked_add_secs(delay).unwrap());
            assert_eq!(chain.replayed(), chain.state);
        }
    }
}
