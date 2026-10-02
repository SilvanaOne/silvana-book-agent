//! Char-boundary-safe string helpers for log and summary text.

/// The first `n` characters of `s` (all of `s` if shorter).
pub(crate) fn short(s: &str, n: usize) -> &str {
    s.char_indices()
        .nth(n)
        .and_then(|(i, _)| s.get(..i))
        .unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::short;

    #[test]
    fn short_cuts_on_characters_not_bytes() {
        assert_eq!(short("abcdef", 3), "abc");
        assert_eq!(short("abc", 8), "abc");
        assert_eq!(short("", 4), "");
        assert_eq!(short("abc", 0), "");
        // 3-byte chars: a byte cut at 8 would land inside the third one
        assert_eq!(short("€€€€€€€€::1220", 8), "€€€€€€€€");
        assert_eq!(short("€€€", 2), "€€");
    }
}
