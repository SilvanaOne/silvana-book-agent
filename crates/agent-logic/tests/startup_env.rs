//! Config loading rejects bad timer settings at startup.
//! Its own test binary: it sets process environment variables.

use agent_logic::config::{BaseConfig, ConfigOverrides};

fn set(k: &str, v: &str) {
    unsafe { std::env::set_var(k, v) }
}

fn unset(k: &str) {
    unsafe { std::env::remove_var(k) }
}

fn load() -> anyhow::Result<BaseConfig> {
    BaseConfig::load_or_defaults_with("/nonexistent/agent.toml", ConfigOverrides::default())
}

// The runtime readers fall back to a default, so only the load-time check stops a bad value
#[test]
fn bad_poll_and_cooldown_values_stop_the_config_load() {
    set("DSO", "dso::1220aa");
    set("PARTY_AGENT", "lp::1220bb");
    set(
        "PARTY_AGENT_PRIVATE_KEY",
        "EB92Q6V2a78t9ppqMuKLppyfzFgyYJciQEVHZKnXAhjEwVpx9aMbQN84SR4ceo3mbLUxQF7TLzaEujaTJnS7eRF",
    );
    set("ORDERBOOK_GRPC_URL", "https://example.test:443");
    set("SYNCHRONIZER_ID", "sync::1220cc");
    set("PARTY_SETTLEMENT_OPERATOR", "op::1220dd");
    set("NODE_NAME", "test-node");
    set("LEDGER_SERVICE_PUBLIC_KEY", &bs58::encode([7u8; 32]).into_string());
    load().expect("the base environment loads");

    for (k, v) in [
        ("FORECAST_POLL_SECS", "0"),
        ("FORECAST_POLL_SECS", "abc"),
        ("LEDGER_UNHEALTHY_COOLDOWN_SECS", "abc"),
        ("LEDGER_UNHEALTHY_COOLDOWN_SECS", "86401"),
    ] {
        set(k, v);
        let err = format!("{:#}", load().unwrap_err());
        assert!(err.contains(k), "{k}={v}: {err}");
        unset(k);
    }
    set("FORECAST_POLL_SECS", "3600");
    set("LEDGER_UNHEALTHY_COOLDOWN_SECS", "0");
    load().expect("in-range values load");
}
