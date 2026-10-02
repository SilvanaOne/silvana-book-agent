//! Startup checks for environment settings that cloud-agent reads lazily,
//! and the env-file loader.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::ledger_client::parse_max_retries;
use crate::payment_queue::PaymentLimits;

/// Fail on an invalid `MAX_RETRIES`, payment worker limit or DvpProposal GC setting, before anything starts.
pub fn validate() -> Result<()> {
    validate_with(&from_process_env)
}

fn from_process_env(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(raw) => Ok(Some(raw)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(anyhow!("{name}: {e}")),
    }
}

fn validate_with(get: &dyn Fn(&str) -> Result<Option<String>>) -> Result<()> {
    parse_max_retries(get("MAX_RETRIES")?.as_deref())?;
    PaymentLimits::from_lookup(get)?;
    crate::dvp_gc_worker::validate_env(get)?;
    Ok(())
}

/// One env file without its byte order mark, checked to parse into variables
/// the environment can hold; an error names the file.
pub fn read_env_file(path: &Path) -> Result<Vec<u8>> {
    read_checked(path).map(|(body, _)| body)
}

/// An env file body without its byte order mark, and the variables it parses into.
type CheckedFile = (Vec<u8>, Vec<(String, String)>);

fn read_checked(path: &Path) -> Result<CheckedFile> {
    let loaded = std::fs::read(path).map_err(anyhow::Error::from).and_then(|bytes| checked_body(&bytes));
    loaded.with_context(|| format!("Failed to load env file {}", path.display()))
}

/// The `.env` the default search loads: in the current directory or the nearest parent.
pub fn find_dotenv() -> Result<Option<PathBuf>> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    Ok(cwd.ancestors().map(|dir| dir.join(".env")).find(|p| p.is_file()))
}

/// # Safety
/// Only while no other thread uses the environment.
#[expect(clippy::disallowed_methods, reason = "env file load while no other thread uses the environment")]
pub unsafe fn apply_env_file(body: &[u8], overwrite: bool) -> Result<()> {
    let mut parsed = 0usize;
    let mut last: Option<String> = None;
    for item in dotenvy::from_read_iter(body) {
        let (k, v) = item.map_err(|e| parse_error(e, parsed, last.as_deref()))?;
        check_env_pair(&k, &v)?;
        if overwrite || std::env::var_os(&k).is_none() {
            // SAFETY: the caller upholds the # Safety contract.
            unsafe { std::env::set_var(&k, v) };
        }
        parsed = parsed.saturating_add(1);
        last = Some(k);
    }
    Ok(())
}

/// A dotenvy error without file text, which can hold values; `parsed` variables came
/// before it, and the last one is named only when it has the shape of a name.
fn parse_error(err: dotenvy::Error, parsed: usize, last: Option<&str>) -> anyhow::Error {
    match err {
        dotenvy::Error::LineParse(..) => {
            let at = match last {
                _ if parsed == 0 => "before the first variable".to_string(),
                Some(k) if looks_like_a_name(k) => format!("after {k} (variable #{parsed})"),
                _ => format!("after variable #{parsed}"),
            };
            anyhow!("a line {at} does not parse; an unclosed quote takes in the lines after it")
        }
        // Io from a byte slice carries no file text
        other => other.into(),
    }
}

/// The usual shape of an env name, `[A-Z_][A-Z0-9_]*`; a value pasted without its
/// name, which dotenvy can read as one, rarely has it.
fn looks_like_a_name(k: &str) -> bool {
    let mut chars = k.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// An env file's variables, read without changing the process environment.
#[derive(Debug, Default)]
pub struct FileVars(Vec<(String, String)>);

impl FileVars {
    /// The variables of `path`; none when it does not exist.
    pub fn load_if_exists(path: &Path) -> Result<Self> {
        if path.exists() {
            read_checked(path).map(|(_, pairs)| Self(pairs))
        } else {
            Ok(Self::default())
        }
    }

    /// `name` as a load without overwrite would leave it, `process` being the
    /// environment: a set variable wins, then the first line that sets it.
    pub fn get_with(&self, name: &str, process: &dyn Fn(&str) -> Option<OsString>) -> Option<String> {
        match process(name) {
            Some(set) => set.into_string().ok(),
            None => self.0.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
        }
    }
}

fn checked_body(bytes: &[u8]) -> Result<CheckedFile> {
    let body = bytes.strip_prefix(&[0xEF_u8, 0xBB, 0xBF]).unwrap_or(bytes);
    let mut pairs: Vec<(String, String)> = Vec::new();
    for item in dotenvy::from_read_iter(body) {
        let pair = item.map_err(|e| parse_error(e, pairs.len(), pairs.last().map(|(k, _)| k.as_str())))?;
        pairs.push(pair);
    }
    check_env_pairs(&pairs)?;
    Ok((body.to_vec(), pairs))
}

/// `set_var` panics on these, so they are refused before anything is set.
fn check_env_pairs(pairs: &[(String, String)]) -> Result<()> {
    pairs.iter().try_for_each(|(k, v)| check_env_pair(k, v))
}

fn check_env_pair(k: &str, v: &str) -> Result<()> {
    if k.is_empty() || k.contains(['=', '\0']) {
        bail!("invalid variable name {k:?}");
    }
    if v.contains('\0') {
        bail!("value of {k} contains a NUL byte");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(vars: Vec<(&'static str, &'static str)>) -> impl Fn(&str) -> Result<Option<String>> {
        move |name| Ok(vars.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_string()))
    }

    #[test]
    fn defaults_and_valid_values_pass() {
        assert!(validate_with(&lookup(vec![])).is_ok());
        assert!(validate_with(&lookup(vec![("MAX_RETRIES", "3"), ("MAX_FEE_WORKERS", "4")])).is_ok());
        assert!(validate_with(&lookup(vec![("MAX_RETRIES", " ")])).is_ok());
    }

    // MAX_RETRIES=0 used to reach a panic once the first submit ran
    #[test]
    fn zero_or_garbage_max_retries_is_rejected() {
        for bad in ["0", "abc", "-1", "1.5"] {
            let err = validate_with(&lookup(vec![("MAX_RETRIES", bad)])).unwrap_err().to_string();
            assert!(err.contains("MAX_RETRIES"), "{bad}: {err}");
        }
    }

    #[test]
    fn payment_limits_are_checked_too() {
        let err = validate_with(&lookup(vec![("MAX_FEE_WORKERS", "0")])).unwrap_err().to_string();
        assert!(err.contains("MAX_FEE_WORKERS"), "{err}");
    }

    // The GC timers and gate used to fall back to defaults, or never close, on bad values
    #[test]
    fn dvp_gc_settings_are_checked_too() {
        let bad = [
            ("DVP_GC_REFRESH_SECS", "0"),
            ("DVP_GC_DELAY_SECS", "2s"),
            ("DVP_GC_MAX_PAUSE_SECS", "30"),
            ("DVP_GC_SAFETY_MARGIN_SECS", "abc"),
            ("DVP_GC_STALE_FORECAST_SECS", "-5"),
            ("DVP_GC_MIN_COEFFICIENT", "NaN"),
            ("DVP_GC_MIN_COEFFICIENT", "inf"),
        ];
        for (name, value) in bad {
            let err = validate_with(&lookup(vec![(name, value)])).unwrap_err().to_string();
            assert!(err.contains(name), "{name}={value}: {err}");
        }
        let good = vec![
            ("DVP_GC_REFRESH_SECS", "60"),
            ("DVP_GC_DELAY_SECS", ""),
            ("DVP_GC_MIN_COEFFICIENT", "0.7"),
            ("DVP_GC_ENABLED", "ture"),
        ];
        assert!(validate_with(&lookup(good)).is_ok());
    }

    // A NUL byte used to reach set_var, which panics on it
    #[test]
    fn a_nul_byte_in_a_name_or_value_is_refused() {
        let pair = |k: &str, v: &str| vec![(k.to_string(), v.to_string())];
        let err = check_env_pairs(&pair("FOO", "a\0b")).unwrap_err().to_string();
        assert_eq!(err, "value of FOO contains a NUL byte");
        assert!(check_env_pairs(&pair("F\0O", "a")).is_err());
        assert!(check_env_pairs(&pair("", "a")).is_err());
        assert!(check_env_pairs(&pair("A=B", "a")).is_err());
        assert!(check_env_pairs(&pair("FOO", "a b=c")).is_ok());
        assert!(checked_body(b"FOO=abc\0\0\0\n").is_err());
        assert!(checked_body(b"FOO=abc\n\0\0\0").is_err());
    }

    #[test]
    fn a_leading_byte_order_mark_is_skipped() {
        let (body, pairs) = checked_body(b"\xEF\xBB\xBFFOO=1\nBAR=\"two\"\n# note\n").unwrap();
        let expected = vec![("FOO".to_string(), "1".to_string()), ("BAR".to_string(), "two".to_string())];
        assert_eq!(pairs, expected);
        assert_eq!(body, b"FOO=1\nBAR=\"two\"\n# note\n");
    }

    #[test]
    fn a_missing_or_malformed_env_file_names_the_file() {
        let dir = std::env::temp_dir().join(format!("cloud-agent-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing.env");
        assert!(format!("{:#}", read_env_file(&missing).unwrap_err()).contains("missing.env"));
        let nul = dir.join("nul.env");
        std::fs::write(&nul, b"FOO=a\0b\n").unwrap();
        let err = format!("{:#}", read_env_file(&nul).unwrap_err());
        assert!(err.contains("nul.env") && err.contains("value of FOO contains a NUL byte"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A parse error used to quote the line, and an unclosed quote takes in every line after it
    #[test]
    fn a_parse_error_shows_no_file_text() {
        let shown = |body: &[u8]| format!("{:#}", checked_body(body).unwrap_err());
        let err = shown(b"FOO=\"x\nKEY=probe-value\n");
        assert!(!err.contains("probe-value") && err.contains("before the first variable"), "{err}");
        let err = shown(b"A=1\nKEY=probeA probeB\n");
        assert!(!err.contains("probeA") && err.contains("after A"), "{err}");
        let err = shown(b"A=1\nKEY=probeB64= x\n");
        assert!(!err.contains("probeB64") && err.contains("after A"), "{err}");
        let err = shown(b"A=1\nB=\"two\nlines\"\nC=\"probe\\q\"\n");
        assert!(!err.contains("probe") && err.contains("after B"), "{err}");
        assert!(checked_body(b"A=1\nB=\"two\nlines\"\n").is_ok());

        // A value pasted without its name parses as a name, so it is never shown
        let err = shown(b"A=1\nprobeTokenAbc==\nB=\"unclosed\n");
        assert!(!err.contains("probeTokenAbc") && err.contains("after variable #2"), "{err}");
        let err = shown(b"A=1\nprobeTokenAbc=\nC=a b\n");
        assert!(!err.contains("probeTokenAbc") && err.contains("after variable #2"), "{err}");
    }

    #[test]
    fn only_a_name_shaped_variable_is_named_in_a_parse_error() {
        let at = |parsed: usize, last: Option<&str>| {
            let line = dotenvy::Error::LineParse("probe-line".to_string(), 3);
            parse_error(line, parsed, last).to_string()
        };
        assert!(at(0, None).contains("a line before the first variable"));
        assert!(at(1, Some("_PARTY_2")).contains("a line after _PARTY_2 (variable #1)"));
        for pasted in ["c2VjcmV0LXRva2Vu", "rust_log", "2FA", "A-B", ""] {
            let err = at(4, Some(pasted));
            assert!(err.contains("a line after variable #4 does not parse") && !err.contains("probe-line"), "{err}");
        }
    }

    #[test]
    fn applying_an_unparsable_body_shows_no_file_text() {
        // SAFETY: the body fails on its first line, before anything is set
        let err = unsafe { apply_env_file(b"FOO=\"x\nKEY=probe-value\n", false) }.unwrap_err();
        let err = format!("{err:#}");
        assert!(!err.contains("probe-value") && err.contains("before the first variable"), "{err}");
    }

    #[test]
    fn an_unreadable_variable_is_an_error() {
        let failing = |name: &str| -> Result<Option<String>> { Err(anyhow!("{name}: not unicode")) };
        assert!(validate_with(&failing).is_err());
    }

    // The same precedence as a load without overwrite: set variables, then earlier lines
    #[test]
    fn file_vars_yield_to_set_variables_and_the_first_line_wins() {
        let line = |k: &str, v: &str| (k.to_string(), v.to_string());
        let vars = FileVars(vec![line("A", "file-a"), line("B", "first"), line("B", "second"), line("E", "file-e")]);
        let process = |name: &str| match name {
            "A" => Some(OsString::from("env-a")),
            "E" => Some(OsString::new()),
            _ => None,
        };
        assert_eq!(vars.get_with("A", &process).as_deref(), Some("env-a"));
        assert_eq!(vars.get_with("B", &process).as_deref(), Some("first"));
        assert_eq!(vars.get_with("E", &process).as_deref(), Some(""), "a set empty value still wins");
        assert_eq!(vars.get_with("C", &process), None);
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let not_unicode = |_: &str| Some(OsString::from_vec(vec![0xFF]));
            assert_eq!(vars.get_with("B", &not_unicode), None, "an unreadable set value is not replaced");
        }
    }

    #[test]
    fn a_missing_env_file_has_no_variables() {
        let missing = std::env::temp_dir().join(format!("cloud-agent-env-absent-{}.env", std::process::id()));
        let vars = FileVars::load_if_exists(&missing).unwrap();
        assert_eq!(vars.get_with("PARTY_AGENT", &|_| None), None);
    }
}
