//! The one rule for the characters kept out of log text.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

/// Whether `c` is unsafe in a log line: a control character, a line separator or a bidi mark.
pub(crate) fn is_log_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{061C}' | '\u{200E}' | '\u{200F}')
        || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::is_log_unsafe;
    use crate::control::intent::{ContentRefusal, PackageRefusal, UnmatchedHash};

    #[test]
    fn controls_separators_and_bidi_marks_are_unsafe() {
        let hidden = [
            '\n', '\r', '\t', '\u{1b}', '\u{7f}', '\u{85}', '\u{2028}', '\u{2029}', '\u{061C}', '\u{200E}', '\u{200F}',
            '\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}',
        ];
        for c in hidden {
            assert!(is_log_unsafe(c), "{c:?}");
        }
        for c in ['a', ' ', '\u{e9}', '\u{200D}', '\u{2027}', '\u{202F}', '\u{2065}', '\u{206A}'] {
            assert!(!is_log_unsafe(c), "{c:?}");
        }
    }

    #[test]
    fn refusal_reasons_drop_separators_and_bidi_marks() {
        let reason = || "a\u{2028}b\u{2029}c\u{202E}d\u{2066}e\u{200F}f".to_string();
        for shown in [
            ContentRefusal::Unreadable(reason()).to_string(),
            PackageRefusal::Unreadable(reason()).to_string(),
            PackageRefusal::HashUnmatched(UnmatchedHash::Uncomputed(reason())).to_string(),
        ] {
            assert!(shown.contains("a b c d e f"), "{shown:?}");
            assert!(!shown.chars().any(is_log_unsafe), "{shown:?}");
        }
    }
}
