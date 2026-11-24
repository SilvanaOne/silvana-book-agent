//! Per-controller command scopes: the command kinds a direct controller may sign,
//! with optional swap pairs (price floor, size cap) and an LP allowlist.

use super::{PolicyInstrument, Rules, read_instrument, write_instrument};
use crate::canon::values::{Amount, InstrumentRef};
use crate::canon::{Reader, Writer, check_ascending};
use crate::command::CommandKind;
use crate::controller::ControllerId;
use crate::error::{CanonError, CanonErrorKind};
use crate::party::PartyId;
use crate::state::MAX_AGENT_CONTROLLERS;

/// Most scopes in one policy: one per controller an agent may have.
pub const MAX_SCOPES: usize = MAX_AGENT_CONTROLLERS;
/// Most command kinds in one scope.
pub const MAX_SCOPE_COMMANDS: usize = CommandKind::ALL.len();
/// Most swap pairs in one scope.
pub const MAX_SCOPE_PAIRS: usize = 16;
/// Most LPs in one scope.
pub const MAX_SCOPE_LPS: usize = 16;

/// A swap direction a scope allows, with its signed bounds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScopePair {
    pub sell: PolicyInstrument,
    pub buy: PolicyInstrument,
    /// Least amount bought per unit sold, net of fees.
    pub min_price: Amount,
    /// Most sold by one command.
    pub max_sell: Amount,
}

/// What one direct controller may sign; a controller without a scope may sign every command.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CommandScope {
    pub controller: ControllerId,
    /// Strictly ascending; empty allows no command.
    pub commands: Vec<CommandKind>,
    /// Strictly ascending by (sell, buy); empty allows every pair without bounds.
    pub pairs: Vec<ScopePair>,
    /// Strictly ascending; empty allows every LP.
    pub lps: Vec<PartyId>,
}

impl CommandScope {
    pub fn allows(&self, kind: CommandKind) -> bool {
        self.commands.contains(&kind)
    }

    /// The listed pair a swap of `sell` for `buy` falls under; Canton Coin matches only
    /// the `cc` alias, so at most one pair names a swap. Empty `pairs` allows every pair.
    pub fn pair_for(
        &self,
        sell: &InstrumentRef,
        buy: &InstrumentRef,
        cc: &InstrumentRef,
    ) -> Option<&ScopePair> {
        let name = |r: &InstrumentRef| {
            if r == cc {
                PolicyInstrument::Cc
            } else {
                PolicyInstrument::Token(r.clone())
            }
        };
        let (sell, buy) = (name(sell), name(buy));
        self.pairs
            .iter()
            .find(|pair| pair.sell == sell && pair.buy == buy)
    }

    /// True when a swap may settle with `lp`; an empty list allows every LP.
    pub fn lp_allowed(&self, lp: &PartyId) -> bool {
        self.lps.is_empty() || self.lps.contains(lp)
    }
}

/// The scope `rules` give `controller`, if any.
pub fn scope_of<'a>(rules: &'a Rules, controller: &ControllerId) -> Option<&'a CommandScope> {
    rules
        .scopes
        .iter()
        .find(|scope| scope.controller == *controller)
}

/// True when `controller` may send a `kind` command; a scope never grants a capability.
pub fn command_allowed(rules: &Rules, controller: &ControllerId, kind: CommandKind) -> bool {
    scope_of(rules, controller).is_none_or(|scope| scope.allows(kind))
}

fn pairs_widen(old: &[ScopePair], new: &[ScopePair]) -> bool {
    if old.is_empty() {
        return false;
    }
    new.is_empty()
        || new.iter().any(|pair| {
            old.iter()
                .find(|o| o.sell == pair.sell && o.buy == pair.buy)
                .is_none_or(|o| pair.min_price < o.min_price || pair.max_sell > o.max_sell)
        })
}

fn lps_widen(old: &[PartyId], new: &[PartyId]) -> bool {
    !old.is_empty() && (new.is_empty() || new.iter().any(|lp| !old.contains(lp)))
}

fn scope_widens(old: &CommandScope, new: &CommandScope) -> bool {
    if new.commands.iter().any(|kind| !old.allows(*kind)) {
        return true;
    }
    new.allows(CommandKind::Swap)
        && (pairs_widen(&old.pairs, &new.pairs) || lps_widen(&old.lps, &new.lps))
}

/// True when `new` lets some controller sign a command `old` refused.
/// A removed entry widens even when it listed every kind, since later kinds would open.
pub fn scopes_widen(old: &[CommandScope], new: &[CommandScope]) -> bool {
    old.iter().any(|before| {
        new.iter()
            .find(|after| after.controller == before.controller)
            .is_none_or(|after| scope_widens(before, after))
    })
}

fn write_list<T: Ord>(
    w: &mut Writer,
    path: &str,
    items: &[T],
    max: usize,
    write_item: impl Fn(&mut Writer, &str, &T),
) {
    w.count(path, items.len());
    if items.len() > max {
        w.fail(CanonErrorKind::CountOutOfRange);
    }
    let mut previous: Option<&T> = None;
    for (i, item) in items.iter().enumerate() {
        write_item(w, &format!("{path}.{i}"), item);
        if let Some(kind) = check_ascending(previous, item) {
            w.fail(kind);
        }
        previous = Some(item);
    }
}

fn read_list<'a, T: Ord>(
    r: &mut Reader<'a>,
    path: &str,
    max: usize,
    read_item: impl Fn(&mut Reader<'a>, &str) -> Result<T, CanonError>,
) -> Result<Vec<T>, CanonError> {
    let count = r.count(path, 0, max)?;
    let mut items: Vec<T> = Vec::with_capacity(count);
    for i in 0..count {
        let item = read_item(r, &format!("{path}.{i}"))?;
        if let Some(kind) = check_ascending(items.last(), &item) {
            return Err(r.err(kind));
        }
        items.push(item);
    }
    Ok(items)
}

/// `SameInstrument`, or the order of (sell, buy) after the previous pair.
fn pair_refusal(
    previous: Option<&ScopePair>,
    sell: &PolicyInstrument,
    buy: &PolicyInstrument,
) -> Option<CanonErrorKind> {
    if sell == buy {
        return Some(CanonErrorKind::SameInstrument);
    }
    let previous = previous.map(|p| (&p.sell, &p.buy));
    check_ascending(previous.as_ref(), &(sell, buy))
}

fn write_pairs(w: &mut Writer, path: &str, pairs: &[ScopePair]) {
    w.count(path, pairs.len());
    if pairs.len() > MAX_SCOPE_PAIRS {
        w.fail(CanonErrorKind::CountOutOfRange);
    }
    let mut previous: Option<&ScopePair> = None;
    for (k, pair) in pairs.iter().enumerate() {
        let prefix = format!("{path}.{k}");
        write_instrument(w, &format!("{prefix}.sell"), &pair.sell);
        write_instrument(w, &format!("{prefix}.buy"), &pair.buy);
        if let Some(kind) = pair_refusal(previous, &pair.sell, &pair.buy) {
            w.fail(kind);
        }
        previous = Some(pair);
        w.line(&format!("{prefix}.min_price"), &pair.min_price.to_string());
        w.line(&format!("{prefix}.max_sell"), &pair.max_sell.to_string());
    }
}

fn read_pairs(r: &mut Reader<'_>, path: &str) -> Result<Vec<ScopePair>, CanonError> {
    let count = r.count(path, 0, MAX_SCOPE_PAIRS)?;
    let mut pairs: Vec<ScopePair> = Vec::with_capacity(count);
    for k in 0..count {
        let prefix = format!("{path}.{k}");
        let sell = read_instrument(r, &format!("{prefix}.sell"))?;
        let buy = read_instrument(r, &format!("{prefix}.buy"))?;
        if let Some(kind) = pair_refusal(pairs.last(), &sell, &buy) {
            return Err(r.err(kind));
        }
        let min_price = r.value(&format!("{prefix}.min_price"), Amount::parse)?;
        let max_sell = r.value(&format!("{prefix}.max_sell"), Amount::parse)?;
        pairs.push(ScopePair {
            sell,
            buy,
            min_price,
            max_sell,
        });
    }
    Ok(pairs)
}

pub(super) fn write_scopes(w: &mut Writer, scopes: &[CommandScope]) {
    w.count("scopes", scopes.len());
    if scopes.len() > MAX_SCOPES {
        w.fail(CanonErrorKind::CountOutOfRange);
    }
    let mut previous: Option<&ControllerId> = None;
    for (i, scope) in scopes.iter().enumerate() {
        let prefix = format!("scopes.{i}");
        w.line(
            &format!("{prefix}.controller"),
            &scope.controller.to_string(),
        );
        if let Some(kind) = check_ascending(previous, &scope.controller) {
            w.fail(kind);
        }
        previous = Some(&scope.controller);
        write_list(
            w,
            &format!("{prefix}.commands"),
            &scope.commands,
            MAX_SCOPE_COMMANDS,
            |w, key, kind| w.line(key, kind.as_str()),
        );
        write_pairs(w, &format!("{prefix}.pairs"), &scope.pairs);
        write_list(
            w,
            &format!("{prefix}.lps"),
            &scope.lps,
            MAX_SCOPE_LPS,
            |w, key, lp| w.line(key, lp.as_str()),
        );
    }
}

pub(super) fn read_scopes(r: &mut Reader<'_>) -> Result<Vec<CommandScope>, CanonError> {
    let count = r.count("scopes", 0, MAX_SCOPES)?;
    let mut scopes: Vec<CommandScope> = Vec::with_capacity(count);
    for i in 0..count {
        let prefix = format!("scopes.{i}");
        let controller = r.value(&format!("{prefix}.controller"), ControllerId::parse)?;
        if let Some(kind) = check_ascending(scopes.last().map(|s| &s.controller), &controller) {
            return Err(r.err(kind));
        }
        let commands = read_list(
            r,
            &format!("{prefix}.commands"),
            MAX_SCOPE_COMMANDS,
            |r, key| r.value(key, CommandKind::parse),
        )?;
        let pairs = read_pairs(r, &format!("{prefix}.pairs"))?;
        let lps = read_list(r, &format!("{prefix}.lps"), MAX_SCOPE_LPS, |r, key| {
            r.value(key, PartyId::parse)
        })?;
        scopes.push(CommandScope {
            controller,
            commands,
            pairs,
            lps,
        });
    }
    Ok(scopes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::values::{InstrumentId, InstrumentRef};
    use crate::command::CommandKind::{Swap, TransferCc, TransferToken, UsdcxBurn};
    use crate::policy::DEFAULT_LINKED_RULES;
    use crate::testkit;

    fn amount(s: &str) -> Amount {
        Amount::parse(s).unwrap()
    }

    fn party(hint: &str, key: u8) -> PartyId {
        PartyId::parse(&format!(
            "{hint}::{}",
            crate::party::namespace_for_key(&[key; 32])
        ))
        .unwrap()
    }

    fn usdc() -> PolicyInstrument {
        PolicyInstrument::Token(InstrumentRef {
            admin: party("Bank", 2),
            id: InstrumentId::parse("USDCx").unwrap(),
        })
    }

    fn pair(sell: PolicyInstrument, buy: PolicyInstrument, min: &str, max: &str) -> ScopePair {
        ScopePair {
            sell,
            buy,
            min_price: amount(min),
            max_sell: amount(max),
        }
    }

    fn scope(controller: ControllerId, commands: &[CommandKind]) -> CommandScope {
        CommandScope {
            controller,
            commands: commands.to_vec(),
            pairs: Vec::new(),
            lps: Vec::new(),
        }
    }

    fn key() -> ControllerId {
        testkit::ed25519_id(0x21)
    }

    #[test]
    fn no_scope_allows_every_kind_and_a_scope_only_its_own() {
        let mut rules = DEFAULT_LINKED_RULES;
        let passkey = testkit::passkey_id();
        for kind in CommandKind::ALL {
            assert!(command_allowed(&rules, &key(), kind));
        }
        rules.scopes = vec![scope(key(), &[Swap])];
        assert!(command_allowed(&rules, &key(), Swap));
        for kind in [TransferCc, TransferToken, UsdcxBurn] {
            assert!(!command_allowed(&rules, &key(), kind), "{kind}");
            assert!(command_allowed(&rules, &passkey, kind), "{kind}");
        }
        assert_eq!(scope_of(&rules, &key()), rules.scopes.first());
        assert_eq!(scope_of(&rules, &passkey), None);
        rules.scopes = vec![scope(key(), &[])];
        assert!(
            CommandKind::ALL
                .into_iter()
                .all(|kind| !command_allowed(&rules, &key(), kind))
        );
    }

    #[test]
    fn entries_and_kinds_widen_or_narrow() {
        let none: Vec<CommandScope> = Vec::new();
        let swap = vec![scope(key(), &[Swap])];
        let both = vec![scope(key(), &[Swap, TransferCc])];
        let empty = vec![scope(key(), &[])];
        let every = vec![scope(key(), &CommandKind::ALL)];
        // Narrowing: an entry added, kinds dropped, the kinds emptied.
        assert!(!scopes_widen(&none, &swap));
        assert!(!scopes_widen(&none, &every));
        assert!(!scopes_widen(&both, &swap));
        assert!(!scopes_widen(&swap, &empty));
        assert!(!scopes_widen(&swap, &swap));
        // Widening: a kind added, an entry removed (even one listing every kind).
        assert!(scopes_widen(&swap, &both));
        assert!(scopes_widen(&empty, &swap));
        assert!(scopes_widen(&swap, &none));
        assert!(scopes_widen(&every, &none));
        // Narrowing two controllers at once still narrows.
        let other = testkit::passkey_id();
        let sorted = |mut scopes: Vec<CommandScope>| {
            scopes.sort_by_key(|s| s.controller);
            scopes
        };
        let old = sorted(vec![
            scope(key(), &[Swap]),
            scope(other, &[TransferCc, TransferToken]),
        ]);
        let narrower = sorted(vec![scope(key(), &[]), scope(other, &[TransferCc])]);
        assert!(!scopes_widen(&old, &narrower));
        // A document that narrows one controller and widens another widens.
        let mixed = sorted(vec![scope(key(), &[]), scope(other, &CommandKind::ALL)]);
        assert!(scopes_widen(&old, &mixed));
        let dropped = vec![scope(key(), &[])];
        assert!(scopes_widen(&old, &dropped));
    }

    #[test]
    fn pairs_widen_when_they_open_a_pair_or_loosen_a_bound() {
        let with_pairs = |pairs: Vec<ScopePair>| {
            vec![CommandScope {
                pairs,
                ..scope(key(), &[Swap])
            }]
        };
        let cc_usdc = pair(PolicyInstrument::Cc, usdc(), "0.15", "1000");
        let usdc_cc = pair(usdc(), PolicyInstrument::Cc, "6", "150");
        let base = with_pairs(vec![cc_usdc.clone()]);
        let changes: [(&str, Vec<ScopePair>, bool); 8] = [
            ("same", vec![cc_usdc.clone()], false),
            ("pair added", vec![cc_usdc.clone(), usdc_cc.clone()], true),
            ("other pair only", vec![usdc_cc.clone()], true),
            ("all pairs removed", Vec::new(), true),
            (
                "min_price lowered",
                vec![pair(PolicyInstrument::Cc, usdc(), "0.14", "1000")],
                true,
            ),
            (
                "max_sell raised",
                vec![pair(PolicyInstrument::Cc, usdc(), "0.15", "1000.5")],
                true,
            ),
            (
                "min_price raised",
                vec![pair(PolicyInstrument::Cc, usdc(), "0.2", "1000")],
                false,
            ),
            (
                "max_sell lowered",
                vec![pair(PolicyInstrument::Cc, usdc(), "0.15", "10")],
                false,
            ),
        ];
        for (name, pairs, widens) in changes {
            assert_eq!(scopes_widen(&base, &with_pairs(pairs)), widens, "{name}");
        }
        // From every pair to listed ones narrows, and back widens.
        let any = with_pairs(Vec::new());
        assert!(!scopes_widen(&any, &base));
        assert!(scopes_widen(&base, &any));
        let two = with_pairs(vec![cc_usdc.clone(), usdc_cc]);
        assert!(!scopes_widen(&two, &base));
        // Pairs do nothing while the scope does not allow swaps.
        let no_swap = |pairs: Vec<ScopePair>| {
            vec![CommandScope {
                pairs,
                ..scope(key(), &[TransferCc])
            }]
        };
        assert!(!scopes_widen(
            &no_swap(vec![cc_usdc.clone()]),
            &no_swap(Vec::new())
        ));
        assert!(scopes_widen(
            &no_swap(vec![cc_usdc]),
            &with_pairs(Vec::new())
        ));
    }

    #[test]
    fn lps_widen_when_one_is_added_or_the_list_removed() {
        let with_lps = |lps: Vec<PartyId>| {
            vec![CommandScope {
                lps,
                ..scope(key(), &[Swap])
            }]
        };
        let (a, b) = (party("lp-a", 7), party("lp-b", 8));
        let one = with_lps(vec![a.clone()]);
        let two = with_lps(vec![a.clone(), b.clone()]);
        let any = with_lps(Vec::new());
        assert!(scopes_widen(&one, &two));
        assert!(scopes_widen(&one, &any));
        assert!(scopes_widen(&one, &with_lps(vec![b])));
        assert!(!scopes_widen(&two, &one));
        assert!(!scopes_widen(&any, &one));
        assert!(!scopes_widen(&one, &one));
        let no_swap = vec![CommandScope {
            lps: Vec::new(),
            ..scope(key(), &[TransferCc])
        }];
        let no_swap_listed = vec![CommandScope {
            lps: vec![a],
            ..scope(key(), &[TransferCc])
        }];
        assert!(!scopes_widen(&no_swap_listed, &no_swap));
    }

    #[test]
    fn a_swap_falls_under_at_most_one_pair_and_cc_is_named_by_its_alias() {
        let cc = InstrumentRef {
            admin: party("DSO", 1),
            id: InstrumentId::parse("Amulet").unwrap(),
        };
        let usdc_ref = InstrumentRef {
            admin: party("Bank", 2),
            id: InstrumentId::parse("USDCx").unwrap(),
        };
        let explicit_cc = PolicyInstrument::Token(cc.clone());
        let mut pairs = vec![
            pair(PolicyInstrument::Cc, usdc(), "0.15", "1000"),
            pair(explicit_cc, usdc(), "0.01", "99999"),
        ];
        pairs.sort_by(|a, b| (&a.sell, &a.buy).cmp(&(&b.sell, &b.buy)));
        let lp = party("lp-a", 7);
        let limited = CommandScope {
            pairs,
            lps: vec![lp.clone()],
            ..scope(key(), &[Swap])
        };
        let found = limited.pair_for(&cc, &usdc_ref, &cc).unwrap();
        assert_eq!(found.sell, PolicyInstrument::Cc);
        assert_eq!(found.min_price, amount("0.15"));
        assert_eq!(limited.pair_for(&usdc_ref, &cc, &cc), None);
        assert!(limited.lp_allowed(&lp));
        assert!(!limited.lp_allowed(&party("lp-b", 8)));
        let open = CommandScope {
            lps: Vec::new(),
            ..limited.clone()
        };
        assert!(open.lp_allowed(&party("lp-b", 8)));
    }
}
