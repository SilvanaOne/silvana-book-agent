//! Pre-Trade Check Agent — pure rule-engine, no network
//!
//! Validates one prospective order against a TOML rules file. Designed to be
//! called from CI / scripts before a real submission:
//!
//! ```bash
//! agent-pre-trade-check-offline-rules check \
//!     --market CC-USDC --side buy --quantity 10 --price 1.02 \
//!     --rules rules.toml && agent-spot-dca run ...
//! ```
//!
//! Rules file shape (all fields optional):
//!
//! ```toml
//! max_notional_per_order = "5000"
//! max_quantity_per_order = "100"
//! min_price = "0.5"
//! max_price = "5.0"
//! blocked_markets = ["XXX-YYY"]
//! allowed_markets = ["CC-USDC", "BTC-USD"]   # if set, market must be in this list
//! allowed_sides   = ["buy", "sell"]          # if set, side must match
//! ```
//!
//! Exits 0 if all rules pass, 2 if any rule rejects, 1 on usage / IO error.
//! Multi-order mode reads JSON Lines from stdin (one order per line).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::str::FromStr;

const EXIT_REJECTED: i32 = 2;

#[derive(Parser)]
#[command(name = "agent-pre-trade-check-offline-rules")]
#[command(about = "Validate an order against a TOML rules file")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Check {
        #[arg(long)]
        market: String,

        /// "buy" or "sell"
        #[arg(long)]
        side: String,

        #[arg(long)]
        quantity: String,

        #[arg(long)]
        price: String,

        #[arg(long)]
        rules: Option<PathBuf>,
    },
    /// Read JSONL from stdin: one order per line. Exits 2 if ANY line is rejected.
    CheckStdin {
        #[arg(long)]
        rules: Option<PathBuf>,

        /// Also print accepted orders on stdout (one per line)
        #[arg(long)]
        echo: bool,
    },
}

#[derive(Default, Deserialize)]
struct Rules {
    #[serde(default)]
    max_notional_per_order: Option<String>,
    #[serde(default)]
    max_quantity_per_order: Option<String>,
    #[serde(default)]
    min_price: Option<String>,
    #[serde(default)]
    max_price: Option<String>,
    #[serde(default)]
    blocked_markets: Vec<String>,
    #[serde(default)]
    allowed_markets: Vec<String>,
    #[serde(default)]
    allowed_sides: Vec<String>,
}

#[derive(Deserialize)]
struct OrderLine {
    market: String,
    side: String,
    quantity: String,
    price: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Check {
            market,
            side,
            quantity,
            price,
            rules,
        } => {
            let rules = load_rules(rules.as_deref())?;
            let violations = evaluate(&market, &side, &quantity, &price, &rules);
            if violations.is_empty() {
                println!("OK");
                Ok(())
            } else {
                for v in &violations {
                    eprintln!("REJECT: {v}");
                }
                std::process::exit(EXIT_REJECTED);
            }
        }
        Commands::CheckStdin { rules, echo } => {
            let rules = load_rules(rules.as_deref())?;
            let stdin = io::stdin();
            let mut any_rejected = false;
            for line in stdin.lock().lines() {
                let line = line.context("stdin read")?;
                if line.trim().is_empty() {
                    continue;
                }
                let order: OrderLine = match serde_json::from_str(&line) {
                    Ok(o) => o,
                    Err(e) => {
                        eprintln!("REJECT: malformed input ({}): {}", e, line);
                        any_rejected = true;
                        continue;
                    }
                };
                let violations =
                    evaluate(&order.market, &order.side, &order.quantity, &order.price, &rules);
                if violations.is_empty() {
                    if echo {
                        println!("{line}");
                    }
                } else {
                    any_rejected = true;
                    for v in &violations {
                        eprintln!(
                            "REJECT: {} {} {} @ {} — {}",
                            order.market, order.side, order.quantity, order.price, v
                        );
                    }
                }
            }
            if any_rejected {
                std::process::exit(EXIT_REJECTED);
            }
            Ok(())
        }
    }
}

fn load_rules(path: Option<&std::path::Path>) -> Result<Rules> {
    let Some(p) = path else { return Ok(Rules::default()) };
    let body = std::fs::read_to_string(p).with_context(|| format!("read rules from {:?}", p))?;
    toml::from_str(&body).with_context(|| format!("parse rules from {:?}", p))
}

fn evaluate(market: &str, side: &str, quantity: &str, price: &str, rules: &Rules) -> Vec<String> {
    let mut v = Vec::new();

    let side_norm = side.to_lowercase();
    if !rules.allowed_sides.is_empty()
        && !rules.allowed_sides.iter().any(|s| s.eq_ignore_ascii_case(&side_norm))
    {
        v.push(format!("side '{}' not in allowed_sides", side));
    } else if side_norm != "buy" && side_norm != "sell" {
        v.push(format!("side must be 'buy' or 'sell', got '{}'", side));
    }

    if rules.blocked_markets.iter().any(|b| b == market) {
        v.push(format!("market '{}' is blocked", market));
    }
    if !rules.allowed_markets.is_empty() && !rules.allowed_markets.iter().any(|a| a == market) {
        v.push(format!("market '{}' not in allowed_markets", market));
    }

    let qty = parse_decimal(quantity, "quantity", &mut v);
    let px = parse_decimal(price, "price", &mut v);

    if let (Some(q), Some(p)) = (qty, px) {
        let notional = q * p;
        if let Some(limit_str) = &rules.max_notional_per_order {
            if let Some(limit) = parse_decimal(limit_str, "max_notional_per_order", &mut v) {
                if notional > limit {
                    v.push(format!("notional {} > max_notional_per_order {}", notional, limit));
                }
            }
        }
        if let Some(limit_str) = &rules.max_quantity_per_order {
            if let Some(limit) = parse_decimal(limit_str, "max_quantity_per_order", &mut v) {
                if q > limit {
                    v.push(format!("quantity {} > max_quantity_per_order {}", q, limit));
                }
            }
        }
        if let Some(min_str) = &rules.min_price {
            if let Some(min) = parse_decimal(min_str, "min_price", &mut v) {
                if p < min {
                    v.push(format!("price {} < min_price {}", p, min));
                }
            }
        }
        if let Some(max_str) = &rules.max_price {
            if let Some(max) = parse_decimal(max_str, "max_price", &mut v) {
                if p > max {
                    v.push(format!("price {} > max_price {}", p, max));
                }
            }
        }
    }

    v
}

fn parse_decimal(s: &str, label: &str, violations: &mut Vec<String>) -> Option<Decimal> {
    match Decimal::from_str(s) {
        Ok(d) => Some(d),
        Err(_) => {
            violations.push(format!("invalid {} value: {:?}", label, s));
            None
        }
    }
}

