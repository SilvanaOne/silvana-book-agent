//! End-to-end checks of the binary's startup path, run in child processes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn cloud_agent(args: &[&str]) -> (Output, Duration) {
    run_in(&std::env::temp_dir(), &[], args)
}

/// Run the binary in `dir` with only `vars` in its environment.
fn run_in(dir: &Path, vars: &[(&str, &str)], args: &[&str]) -> (Output, Duration) {
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_cloud-agent"))
        .args(args)
        .env_clear()
        .envs(vars.iter().copied())
        .current_dir(dir)
        .output()
        .unwrap();
    (out, started.elapsed())
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn env_file(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("cli-startup-{}-{name}.env", std::process::id()));
    std::fs::write(&path, body).unwrap();
    path
}

/// A URL whose port refuses connections.
fn refused_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port())
}

/// An error exit that is not a panic (exit 101).
fn assert_clean_failure(out: &Output, needle: &str) {
    let err = stderr(out);
    assert!(!out.status.success(), "{err}");
    assert_ne!(out.status.code(), Some(101), "{err}");
    assert!(!err.contains("panicked"), "{err}");
    assert!(err.contains(needle), "expected {needle:?} in: {err}");
}

#[test]
fn generate_private_key_needs_no_config() {
    let (out, _) = cloud_agent(&["generate-private-key"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("PARTY_AGENT_PRIVATE_KEY="), "{stdout}");
    assert!(stdout.contains("PARTY_AGENT_PUBLIC_KEY="), "{stdout}");
}

#[test]
fn info_network_uses_the_env_file_and_fails_cleanly() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let path = env_file("network", &format!("ORDERBOOK_GRPC_URL=http://127.0.0.1:{port}\n"));
    let (out, took) = cloud_agent(&["--env-file", path.to_str().unwrap(), "info", "network"]);
    let _ = std::fs::remove_file(&path);
    let err = stderr(&out);
    assert!(!out.status.success());
    assert!(err.contains("Error:"), "{err}");
    assert!(!err.contains("ORDERBOOK_GRPC_URL env var is required"), "{err}");
    assert!(!err.contains("panicked"), "{err}");
    assert!(took < Duration::from_secs(30), "{took:?}");
}

// A path under a regular file cannot be a directory, even for root
#[test]
fn a_bad_log_directory_stops_startup() {
    let file = std::env::temp_dir().join(format!("cli-startup-{}-log-file", std::process::id()));
    std::fs::write(&file, b"x").unwrap();
    let log_dir = file.join("logs");
    let vars = [("LOG_DESTINATION", "file"), ("LOG_DIR", log_dir.to_str().unwrap())];
    let (out, _) = run_in(&std::env::temp_dir(), &vars, &["generate-private-key"]);
    let _ = std::fs::remove_file(&file);
    assert_clean_failure(&out, "cannot open log directory");
}

#[test]
fn a_missing_env_file_stops_startup() {
    let (out, _) = cloud_agent(&["--env-file", "/nonexistent/cli-startup.env", "info", "network"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("env file not found"), "{}", stderr(&out));
}

#[test]
fn an_invalid_fill_amount_is_a_usage_error() {
    let (out, _) = cloud_agent(&["buy", "--market", "M", "--amount=NaN"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("must be a finite number above 0"), "{}", stderr(&out));
}

// tokio used to panic while reading a bad TOKIO_WORKER_THREADS itself
#[test]
fn a_bad_worker_thread_count_is_a_startup_error() {
    let body = format!("ORDERBOOK_GRPC_URL={}\nTOKIO_WORKER_THREADS=abc\n", refused_url());
    let path = env_file("workers", &body);
    let (out, _) = cloud_agent(&["--env-file", path.to_str().unwrap(), "info", "network"]);
    let _ = std::fs::remove_file(&path);
    assert_clean_failure(&out, "TOKIO_WORKER_THREADS");
}

// set_var used to panic on a NUL byte from the env file
#[test]
fn a_nul_byte_in_the_env_file_is_a_startup_error() {
    let body = format!("ORDERBOOK_GRPC_URL={}\nFOO=a\0b\n", refused_url());
    let path = env_file("nul", &body);
    let (out, _) = cloud_agent(&["--env-file", path.to_str().unwrap(), "info", "network"]);
    let _ = std::fs::remove_file(&path);
    assert_clean_failure(&out, path.to_str().unwrap());
    assert!(stderr(&out).contains("NUL byte"), "{}", stderr(&out));
}

#[test]
fn the_default_env_file_fills_in_unset_variables_and_is_checked_too() {
    let dir = std::env::temp_dir().join(format!("cli-startup-{}-dotenv", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".env"), "ORDERBOOK_GRPC_URL=http://127.0.0.1:1\n").unwrap();
    let (out, _) = run_in(&dir, &[("ORDERBOOK_GRPC_URL", "http://127.0.0.1:2")], &["info", "network"]);
    let err = stderr(&out);
    assert!(err.contains("127.0.0.1:2") && !err.contains("127.0.0.1:1"), "a set variable wins: {err}");
    let (out, _) = run_in(&dir, &[], &["info", "network"]);
    assert!(stderr(&out).contains("127.0.0.1:1"), "{}", stderr(&out));

    std::fs::write(dir.join(".env"), "ORDERBOOK_GRPC_URL=http://127.0.0.1:1\nFOO=a\0b\n").unwrap();
    let (out, _) = run_in(&dir, &[], &["info", "network"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_clean_failure(&out, ".env");
}

// The startup error used to quote the env file from the bad line to its end, key included
#[test]
fn a_malformed_env_file_does_not_print_its_values() {
    let dir = std::env::temp_dir().join(format!("cli-startup-{}-dotenv-quote", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".env"), "FOO=\"x\nPARTY_AGENT_PRIVATE_KEY=probe-value\n").unwrap();
    let (out, _) = run_in(&dir, &[], &["info", "network"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_clean_failure(&out, ".env");
    assert!(!stderr(&out).contains("probe-value"), "{}", stderr(&out));
}

// A ${VAR} used to take the shell's value, not the one the env file set just before
#[test]
fn an_env_file_expands_its_own_earlier_lines() {
    let path = env_file("subst", "PROBE_PORT=1\nORDERBOOK_GRPC_URL=http://127.0.0.1:${PROBE_PORT}\n");
    let (out, _) = run_in(&std::env::temp_dir(), &[("PROBE_PORT", "2")], &["--env-file", path.to_str().unwrap(), "info", "network"]);
    let _ = std::fs::remove_file(&path);
    let err = stderr(&out);
    assert!(err.contains("127.0.0.1:1") && !err.contains("127.0.0.1:2"), "{err}");
}

// The default .env keeps the first value of a repeated name, and ${VAR} sees that value
#[test]
fn the_default_env_file_expands_the_value_it_kept() {
    let dir = std::env::temp_dir().join(format!("cli-startup-{}-dotenv-repeat", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".env"), "A=1\nA=2\nORDERBOOK_GRPC_URL=http://127.0.0.1:${A}\n").unwrap();
    let (out, _) = run_in(&dir, &[], &["info", "network"]);
    let _ = std::fs::remove_dir_all(&dir);
    let err = stderr(&out);
    assert!(err.contains("127.0.0.1:1") && !err.contains("127.0.0.1:2"), "{err}");
}

/// A new empty folder under the temp dir.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cli-startup-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_config_folder_env_file_overrides_set_variables() {
    let dir = scratch_dir("config-dir-env");
    std::fs::write(dir.join(".env"), "ORDERBOOK_GRPC_URL=http://127.0.0.1:1\n").unwrap();
    let shell = [("ORDERBOOK_GRPC_URL", "http://127.0.0.1:2")];
    let (out, _) = run_in(&std::env::temp_dir(), &shell, &["--config-dir", dir.to_str().unwrap(), "info", "network"]);
    let _ = std::fs::remove_dir_all(&dir);
    let err = stderr(&out);
    assert!(err.contains("127.0.0.1:1") && !err.contains("127.0.0.1:2"), "the folder's .env wins: {err}");
}

#[test]
fn a_config_folder_without_an_env_file_does_not_search_its_parents() {
    let parent = scratch_dir("config-dir-parent");
    std::fs::write(parent.join(".env"), "ORDERBOOK_GRPC_URL=http://127.0.0.1:1\n").unwrap();
    std::fs::create_dir_all(parent.join("agent")).unwrap();
    let (out, _) = run_in(&parent, &[], &["--config-dir", "agent", "info", "network"]);
    let _ = std::fs::remove_dir_all(&parent);
    assert_clean_failure(&out, "env file not found");
    assert!(stderr(&out).contains(".env") && !stderr(&out).contains("127.0.0.1:1"), "{}", stderr(&out));
}

#[test]
fn a_missing_config_folder_stops_startup() {
    let (out, _) = cloud_agent(&["--config-dir", "/nonexistent/cli-startup-agent", "generate-private-key"]);
    assert_clean_failure(&out, "--config-dir /nonexistent/cli-startup-agent: ");
    assert!(out.stdout.is_empty(), "no key is printed");
    let file = env_file("config-dir-file", "");
    let (out, _) = cloud_agent(&["--config-dir", file.to_str().unwrap(), "generate-private-key"]);
    let _ = std::fs::remove_file(&file);
    assert_clean_failure(&out, "not a folder");
}

#[test]
fn relative_paths_resolve_in_the_config_folder() {
    let dir = scratch_dir("config-dir-paths");
    let folder = dir.to_str().unwrap();
    std::fs::write(dir.join(".env"), "LOG_DESTINATION=file\n").unwrap();
    let (out, _) = run_in(&std::env::temp_dir(), &[], &["--config-dir", folder, "generate-private-key"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let logs: Vec<String> = std::fs::read_dir(dir.join("logs"))
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    assert!(logs.iter().any(|n| n.starts_with("cloud-agent.")), "logs go to the folder: {logs:?}");

    // -c is read inside the folder: a malformed file there fails to parse
    std::fs::write(dir.join("custom.toml"), "not toml [\n").unwrap();
    let (out, _) = run_in(&std::env::temp_dir(), &[], &["--config-dir", folder, "-c", "custom.toml", "agent"]);
    assert_clean_failure(&out, "Failed to parse custom.toml");

    // --env-file is read inside the folder and replaces its .env
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub").join("agent.env"), "ORDERBOOK_GRPC_URL=http://127.0.0.1:1\n").unwrap();
    let (out, _) = run_in(&std::env::temp_dir(), &[], &["--config-dir", folder, "--env-file", "sub/agent.env", "info", "network"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(stderr(&out).contains("127.0.0.1:1"), "{}", stderr(&out));
}

// Console logs share stdout, so the folder notice goes to stderr and the keys can go to a file
#[test]
fn the_config_folder_notice_stays_off_stdout() {
    let dir = scratch_dir("config-dir-stdout");
    std::fs::write(dir.join(".env"), "FOO=bar\n").unwrap();
    let (out, _) = run_in(&std::env::temp_dir(), &[], &["--config-dir", dir.to_str().unwrap(), "generate-private-key"]);
    let _ = std::fs::remove_dir_all(&dir);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let names: Vec<&str> = stdout.lines().map(|l| l.split_once('=').map_or(l, |(name, _)| name)).collect();
    assert_eq!(names, ["PARTY_AGENT_PRIVATE_KEY", "PARTY_AGENT_PUBLIC_KEY"]);
    assert!(err.contains("Config folder: "), "{err}");
}

#[test]
fn onboarding_into_a_config_folder_refuses_testnet_before_writing() {
    let dir = scratch_dir("config-dir-onboard");
    let args = ["--config-dir", dir.to_str().unwrap(), "onboard", "--network", "testnet", "--agent-name", "a", "--invite-code", "i"];
    let (out, _) = run_in(&std::env::temp_dir(), &[], &args);
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert_clean_failure(&out, "testnet has no rpc yet");
    assert!(left.is_empty(), "nothing is written: {left:?}");
}

/// An env file with a full agent identity, plus `extra`.
fn agent_env(name: &str, extra: &str) -> PathBuf {
    let (keys, _) = cloud_agent(&["generate-private-key"]);
    let keys = String::from_utf8(keys.stdout).unwrap();
    let value = |name: &str| {
        let prefix = format!("{name}=");
        keys.lines().find_map(|l| l.strip_prefix(prefix.as_str())).unwrap().to_string()
    };
    let body = format!(
        "ORDERBOOK_GRPC_URL={}\nDSO=dso::1220aa\nPARTY_SETTLEMENT_OPERATOR=op::1220dd\n\
         PARTY_ORDERBOOK_FEE=fee::1220ee\nPARTY_AGENT=lp::1220bb\nPARTY_AGENT_PRIVATE_KEY={}\n\
         LEDGER_SERVICE_PUBLIC_KEY={}\nSYNCHRONIZER_ID=sync::1220cc\nNODE_NAME=test-node\n{extra}\n",
        refused_url(),
        value("PARTY_AGENT_PRIVATE_KEY"),
        value("PARTY_AGENT_PUBLIC_KEY"),
    );
    env_file(name, &body)
}

// The runtime readers fall back to a default, so only the startup check stops these
#[test]
fn bad_retry_and_timer_settings_stop_the_agent_and_the_fill_loop() {
    let toml = std::env::temp_dir().join(format!("cli-startup-{}-agent.toml", std::process::id()));
    std::fs::write(&toml, "").unwrap();
    let bad = [
        ("MAX_RETRIES", "0"),
        ("FORECAST_POLL_SECS", "0"),
        ("LEDGER_UNHEALTHY_COOLDOWN_SECS", "abc"),
        ("DVP_GC_REFRESH_SECS", "0"),
        ("DVP_GC_MIN_COEFFICIENT", "NaN"),
        ("DVP_GC_DELAY_SECS", "2s"),
    ];
    for (k, v) in bad {
        let path = agent_env(&format!("bad-{k}"), &format!("{k}={v}"));
        let env = path.to_str().unwrap();
        let (fill, fill_took) = cloud_agent(&[
            "--env-file",
            env,
            "buy",
            "--market",
            "CC-USDC",
            "--amount",
            "1",
        ]);
        let (agent, agent_took) = cloud_agent(&[
            "--env-file",
            env,
            "--config",
            toml.to_str().unwrap(),
            "agent",
        ]);
        let _ = std::fs::remove_file(&path);
        assert_clean_failure(&fill, k);
        assert_clean_failure(&agent, k);
        // The checks run before the instrument registry wait, which this rpc would make long
        assert!(fill_took < Duration::from_secs(30), "{k}: {fill_took:?}");
        assert!(agent_took < Duration::from_secs(30), "{k}: {agent_took:?}");
    }
    let _ = std::fs::remove_file(&toml);
}
