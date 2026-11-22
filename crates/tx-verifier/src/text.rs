//! Char-boundary-safe string helpers for log and summary text, and the
//! logical form of synchronizer ids.

/// The first `n` characters of `s` (all of `s` if shorter).
pub(crate) fn short(s: &str, n: usize) -> &str {
    s.char_indices()
        .nth(n)
        .and_then(|(i, _)| s.get(..i))
        .unwrap_or(s)
}

/// The logical id of synchronizer id `id`: one trailing `::<protocol>-<serial>` of a
/// physical id is dropped; any other id is returned unchanged.
pub(crate) fn logical_synchronizer_id(id: &str) -> &str {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match id.rsplit_once("::") {
        Some((head, suffix))
            if head.contains("::")
                && suffix
                    .split_once('-')
                    .is_some_and(|(p, s)| digits(p) && digits(s)) =>
        {
            head
        }
        _ => id,
    }
}

#[cfg(test)]
mod tests {
    use super::{logical_synchronizer_id, short};

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

    #[test]
    fn a_physical_synchronizer_id_loses_one_protocol_serial_suffix() {
        const L: &str = "global-domain::1220be58";
        assert_eq!(logical_synchronizer_id(&format!("{L}::37-8")), L);
        assert_eq!(logical_synchronizer_id(&format!("{L}::0-0")), L);
        assert_eq!(
            logical_synchronizer_id(&format!("{L}::36-7::37-8")),
            format!("{L}::36-7")
        );
        for id in [
            L.to_string(),
            format!("{L}::37"),
            format!("{L}::a-0"),
            format!("{L}::-"),
            format!("{L}::37-"),
            format!("{L}::-8"),
            format!("{L}::3-4-5"),
            format!("{L}::36-7::x"),
            // non-ASCII digits
            format!("{L}::\u{663}-\u{664}"),
            "x::3-4".to_string(),
            "::3-4".to_string(),
            "global-domain".to_string(),
            String::new(),
        ] {
            assert_eq!(logical_synchronizer_id(&id), id, "{id:?}");
        }
    }
}
