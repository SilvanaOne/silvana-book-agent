//! Plain-language lines describing a rule set, for the CLI and the `--force` warning.

use crate::policy::scopes::{CommandScope, ScopePair};
use crate::policy::{Allowlist, Limits, PolicyInstrument, Rules, Unlisted};

fn duration(secs: u64) -> String {
    if secs == 0 {
        return "none".to_owned();
    }
    match (secs.checked_rem(3600), secs.checked_rem(60)) {
        (Some(0), _) => format!("{} h", secs.checked_div(3600).unwrap_or(0)),
        (_, Some(0)) => format!("{} min", secs.checked_div(60).unwrap_or(0)),
        _ => format!("{secs} s"),
    }
}

fn instrument(instrument: &PolicyInstrument) -> String {
    match instrument {
        PolicyInstrument::Cc => "cc (Canton Coin)".to_owned(),
        PolicyInstrument::Token(r) => format!("{} ({})", r.id, r.admin),
    }
}

fn allowlist<T>(list: &Allowlist<T>, show: impl Fn(&T) -> String) -> String {
    if list.any {
        return "any".to_owned();
    }
    if list.items.is_empty() {
        return "none".to_owned();
    }
    list.items.iter().map(show).collect::<Vec<_>>().join(", ")
}

fn limits(limits: &Limits) -> String {
    let others = match limits.unlisted {
        Unlisted::Unlimited => "unlimited",
        Unlisted::Deny => "denied",
    };
    if limits.items.is_empty() {
        return others.to_owned();
    }
    let listed: Vec<String> = limits
        .items
        .iter()
        .map(|item| format!("{} up to {}", instrument(&item.instrument), item.amount))
        .collect();
    format!("{}; others {others}", listed.join("; "))
}

fn cap(amount: Option<crate::canon::values::Amount>, unit: &str) -> String {
    amount.map_or("unlimited".to_owned(), |a| format!("up to {a} {unit}"))
}

fn switch(enabled: bool) -> &'static str {
    if enabled { "allowed" } else { "disabled" }
}

fn pair(pair: &ScopePair) -> String {
    format!(
        "{} to {}, at least {} bought per unit sold, at most {} sold per command",
        instrument(&pair.sell),
        instrument(&pair.buy),
        pair.min_price,
        pair.max_sell
    )
}

fn scope_lines(scope: &CommandScope) -> [String; 3] {
    let fingerprint = scope.controller.fingerprint();
    let commands = if scope.commands.is_empty() {
        "none".to_owned()
    } else {
        let kinds: Vec<&str> = scope.commands.iter().map(|kind| kind.as_str()).collect();
        kinds.join(", ")
    };
    let pairs = if scope.pairs.is_empty() {
        "any pair, unbounded".to_owned()
    } else {
        scope.pairs.iter().map(pair).collect::<Vec<_>>().join("; ")
    };
    let lps = if scope.lps.is_empty() {
        "any".to_owned()
    } else {
        scope
            .lps
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    [
        format!("Command scope {fingerprint}: {commands}."),
        format!("Swap pairs {fingerprint}: {pairs}."),
        format!("Swap LPs {fingerprint}: {lps}."),
    ]
}

fn scopes(scopes: &[CommandScope]) -> Vec<String> {
    if scopes.is_empty() {
        let line = "Command scopes: none; every direct controller may send every command.";
        return vec![line.to_owned()];
    }
    let mut lines: Vec<String> = scopes.iter().flat_map(scope_lines).collect();
    lines.push("Direct controllers without a scope: every command.".to_owned());
    lines
}

/// One line per rule, in a fixed order; the same rules always give the same lines.
pub fn describe_rules(rules: &Rules) -> Vec<String> {
    let trading = &rules.trading;
    let transfers = &rules.transfers;
    let transfer_mode = if !transfers.enabled {
        "disabled"
    } else if transfers.directed_only {
        "only when a controller directs them"
    } else {
        "allowed without direction"
    };
    let mut lines = vec![
        if rules.paused {
            "Paused: only maintenance, fee consents and unwinding run.".to_owned()
        } else {
            "Not paused.".to_owned()
        },
        format!(
            "Widening delays: transfers {}, trading {}, other {}.",
            duration(rules.delays.transfers_secs),
            duration(rules.delays.trading_secs),
            duration(rules.delays.other_secs)
        ),
        format!(
            "Maintenance: {}; traffic purchases {} per day.",
            switch(rules.maintenance.enabled),
            cap(rules.maintenance.traffic_cc_per_day, "CC")
        ),
        format!(
            "Trading: {}; liquidity provision {}.",
            switch(trading.enabled),
            switch(trading.lp_enabled)
        ),
        format!(
            "Trading markets: {}{}.",
            allowlist(&trading.markets, |m| m.to_string()),
            if trading.markets.any {
                ""
            } else {
                "; with a list instead of any, every swap command is refused"
            }
        ),
        format!(
            "Trading instruments: {}.",
            allowlist(&trading.instruments, instrument)
        ),
        format!("Per trade: {}.", limits(&trading.per_trade)),
        format!("Trading per day: {}.", limits(&trading.per_day)),
        format!("Transfers: {transfer_mode}."),
        format!(
            "Transfer recipients: {}.",
            allowlist(&transfers.recipients, |p| p.to_string())
        ),
        format!(
            "Ethereum destinations: {}.",
            allowlist(&transfers.eth_destinations, |a| a.to_string())
        ),
        format!("Per transfer: {}.", limits(&transfers.per_transfer)),
        format!("Transfers per day: {}.", limits(&transfers.per_day)),
        format!(
            "Locks and venues: {}; per lock: {}.",
            switch(rules.locks.enabled),
            limits(&rules.locks.per_lock)
        ),
        format!("USDCx: {}.", switch(rules.usdcx.enabled)),
        format!(
            "Fees: {} per consent; {} per day.",
            cap(rules.fees.cc_per_consent, "CC"),
            cap(rules.fees.cc_per_day, "CC")
        ),
        format!(
            "Packages: {}.",
            allowlist(&rules.packages, ToString::to_string)
        ),
    ];
    lines.extend(scopes(&rules.scopes));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::values::{Amount, InstrumentId, InstrumentRef, MarketId, PackageId};
    use crate::command::CommandKind;
    use crate::party::PartyId;
    use crate::policy::{DEFAULT_LINKED_RULES, LimitItem};

    #[test]
    fn the_defaults_read_as_pinned() {
        assert_eq!(
            describe_rules(&DEFAULT_LINKED_RULES),
            vec![
                "Not paused.",
                "Widening delays: transfers 24 h, trading none, other none.",
                "Maintenance: allowed; traffic purchases unlimited per day.",
                "Trading: allowed; liquidity provision allowed.",
                "Trading markets: any.",
                "Trading instruments: any.",
                "Per trade: unlimited.",
                "Trading per day: unlimited.",
                "Transfers: only when a controller directs them.",
                "Transfer recipients: any.",
                "Ethereum destinations: any.",
                "Per transfer: unlimited.",
                "Transfers per day: unlimited.",
                "Locks and venues: allowed; per lock: unlimited.",
                "USDCx: allowed.",
                "Fees: unlimited per consent; unlimited per day.",
                "Packages: any.",
                "Command scopes: none; every direct controller may send every command.",
            ]
        );
    }

    #[test]
    fn packages_and_scopes_are_listed_in_full() {
        let bank = PartyId::parse(&format!(
            "Bank::{}",
            crate::party::namespace_for_key(&[2; 32])
        ))
        .unwrap();
        let lp = PartyId::parse(&format!(
            "lp-1::{}",
            crate::party::namespace_for_key(&[7; 32])
        ))
        .unwrap();
        let usdc = PolicyInstrument::Token(InstrumentRef {
            admin: bank.clone(),
            id: InstrumentId::parse("USDCx").unwrap(),
        });
        let key = crate::controller::ControllerId([0xab; 32]);
        let stopped = crate::controller::ControllerId([0xcd; 32]);
        let mut rules = DEFAULT_LINKED_RULES;
        rules.packages = Allowlist {
            any: false,
            items: vec![PackageId([1; 32]), PackageId([2; 32])],
        };
        rules.scopes = vec![
            CommandScope {
                controller: key,
                commands: vec![CommandKind::Swap, CommandKind::TransferCc],
                pairs: vec![ScopePair {
                    sell: PolicyInstrument::Cc,
                    buy: usdc,
                    min_price: Amount::parse("0.15").unwrap(),
                    max_sell: Amount::parse("1000").unwrap(),
                }],
                lps: vec![lp.clone()],
            },
            CommandScope {
                controller: stopped,
                commands: Vec::new(),
                pairs: Vec::new(),
                lps: Vec::new(),
            },
        ];
        let lines = describe_rules(&rules);
        assert_eq!(
            lines[16],
            format!("Packages: {}, {}.", "01".repeat(32), "02".repeat(32))
        );
        assert_eq!(
            &lines[17..],
            [
                "Command scope ABAB ABAB ABAB ABAB ABAB: swap, transfer_cc.".to_owned(),
                format!(
                    "Swap pairs ABAB ABAB ABAB ABAB ABAB: cc (Canton Coin) to USDCx ({bank}), \
                     at least 0.15 bought per unit sold, at most 1000 sold per command."
                ),
                format!("Swap LPs ABAB ABAB ABAB ABAB ABAB: {lp}."),
                "Command scope CDCD CDCD CDCD CDCD CDCD: none.".to_owned(),
                "Swap pairs CDCD CDCD CDCD CDCD CDCD: any pair, unbounded.".to_owned(),
                "Swap LPs CDCD CDCD CDCD CDCD CDCD: any.".to_owned(),
                "Direct controllers without a scope: every command.".to_owned(),
            ]
        );
    }

    #[test]
    fn restrictive_rules_name_every_limit() {
        let bank = PartyId::parse(&format!(
            "Bank::{}",
            crate::party::namespace_for_key(&[2; 32])
        ))
        .unwrap();
        let usdc = PolicyInstrument::Token(InstrumentRef {
            admin: bank.clone(),
            id: InstrumentId::parse("USDCx").unwrap(),
        });
        let mut rules = DEFAULT_LINKED_RULES;
        rules.paused = true;
        rules.delays.trading_secs = 90;
        rules.delays.other_secs = 1800;
        rules.maintenance.traffic_cc_per_day = Some(Amount::parse("25.5").unwrap());
        rules.trading.lp_enabled = false;
        rules.trading.markets = Allowlist {
            any: false,
            items: vec![MarketId::parse("CC/USDCx").unwrap()],
        };
        rules.trading.instruments = Allowlist {
            any: false,
            items: Vec::new(),
        };
        rules.trading.per_trade = Limits {
            unlisted: Unlisted::Deny,
            items: vec![
                LimitItem {
                    instrument: usdc.clone(),
                    amount: Amount::parse("1000").unwrap(),
                },
                LimitItem {
                    instrument: PolicyInstrument::Cc,
                    amount: Amount::ZERO,
                },
            ],
        };
        rules.transfers.directed_only = false;
        rules.transfers.enabled = false;
        rules.fees.cc_per_consent = Some(Amount::parse("0.5").unwrap());
        let lines = describe_rules(&rules);
        assert_eq!(
            lines[0],
            "Paused: only maintenance, fee consents and unwinding run."
        );
        assert_eq!(
            lines[1],
            "Widening delays: transfers 24 h, trading 90 s, other 30 min."
        );
        assert_eq!(
            lines[2],
            "Maintenance: allowed; traffic purchases up to 25.5 CC per day."
        );
        assert_eq!(lines[3], "Trading: allowed; liquidity provision disabled.");
        assert_eq!(
            lines[4],
            "Trading markets: CC/USDCx; with a list instead of any, every swap command is refused."
        );
        assert_eq!(lines[5], "Trading instruments: none.");
        assert_eq!(
            lines[6],
            format!(
                "Per trade: USDCx ({bank}) up to 1000; cc (Canton Coin) up to 0; others denied."
            )
        );
        assert_eq!(lines[8], "Transfers: disabled.");
        assert_eq!(
            lines[15],
            "Fees: up to 0.5 CC per consent; unlimited per day."
        );
        assert_eq!(describe_rules(&rules), lines);
    }
}
