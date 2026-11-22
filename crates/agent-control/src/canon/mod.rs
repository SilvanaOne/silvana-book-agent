//! Line-based canonical text: `key=value` lines in a fixed order, LF-terminated.
//! `Writer` and `Reader` are the only code that produces or consumes it.

pub mod codes;
pub mod time;
pub mod values;

use std::cmp::Ordering;

use crate::MIN_DOCUMENT_BYTES;
use crate::error::{CanonError, CanonErrorKind, ValueError};

fn is_value_byte(b: u8) -> bool {
    (0x20..=0x7e).contains(&b)
}

fn is_key_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.'
}

fn check_size(len: usize, max: usize) -> Result<(), CanonError> {
    if len < MIN_DOCUMENT_BYTES {
        return Err(CanonError::new(0, CanonErrorKind::TooShort));
    }
    if len > max {
        return Err(CanonError::new(0, CanonErrorKind::TooLarge));
    }
    Ok(())
}

/// `DuplicateItem` or `UnsortedList` unless `next` sorts strictly after `prev`.
pub(crate) fn check_ascending<T: Ord>(prev: Option<&T>, next: &T) -> Option<CanonErrorKind> {
    match prev.map(|p| p.cmp(next)) {
        Some(Ordering::Equal) => Some(CanonErrorKind::DuplicateItem),
        Some(Ordering::Greater) => Some(CanonErrorKind::UnsortedList),
        _ => None,
    }
}

/// Builds a document; the first error sticks and is returned by `finish`.
pub(crate) struct Writer {
    out: String,
    lines: usize,
    max: usize,
    error: Option<CanonError>,
}

impl Writer {
    pub(crate) fn new(scheme: &str, max: usize) -> Self {
        let mut writer = Self {
            out: String::with_capacity(512),
            lines: 0,
            max,
            error: None,
        };
        writer.line("scheme", scheme);
        writer
    }

    /// A required line; an empty value is an error.
    pub(crate) fn line(&mut self, key: &str, value: &str) {
        self.push(key, value, false);
    }

    /// An optional line; `None` writes an empty value.
    pub(crate) fn opt(&mut self, key: &str, value: Option<&str>) {
        self.push(key, value.unwrap_or(""), true);
    }

    pub(crate) fn count(&mut self, path: &str, n: usize) {
        self.push(&format!("{path}.count"), &n.to_string(), false);
    }

    /// Records an error at the line written last.
    pub(crate) fn fail(&mut self, kind: CanonErrorKind) {
        if self.error.is_none() {
            self.error = Some(CanonError::new(self.lines, kind));
        }
    }

    fn push(&mut self, key: &str, value: &str, allow_empty: bool) {
        self.lines = self.lines.saturating_add(1);
        if self.error.is_some() {
            return;
        }
        let refusal = if key.is_empty() || !key.bytes().all(is_key_byte) {
            Some(CanonErrorKind::MalformedLine)
        } else if value.is_empty() && !allow_empty {
            Some(CanonErrorKind::EmptyValue)
        } else if !value.bytes().all(is_value_byte) {
            Some(CanonErrorKind::InvalidByte)
        } else {
            None
        };
        if let Some(kind) = refusal {
            self.error = Some(CanonError::new(self.lines, kind));
            return;
        }
        self.out.push_str(key);
        self.out.push('=');
        self.out.push_str(value);
        self.out.push('\n');
    }

    pub(crate) fn finish(self) -> Result<Vec<u8>, CanonError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        check_size(self.out.len(), self.max)?;
        Ok(self.out.into_bytes())
    }
}

/// Reads a document line by line, requiring each expected key in turn.
pub(crate) struct Reader<'a> {
    lines: Vec<&'a str>,
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Checks size, bytes, line structure and the scheme line.
    pub(crate) fn new(bytes: &'a [u8], scheme: &str, max: usize) -> Result<Self, CanonError> {
        check_size(bytes.len(), max)?;
        let mut line = 1usize;
        for &b in bytes {
            if b == b'\n' {
                line = line.saturating_add(1);
            } else if !is_value_byte(b) {
                return Err(CanonError::new(line, CanonErrorKind::InvalidByte));
            }
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| CanonError::new(0, CanonErrorKind::InvalidByte))?;
        let Some(body) = text.strip_suffix('\n') else {
            return Err(CanonError::new(line, CanonErrorKind::MissingFinalNewline));
        };
        let lines: Vec<&'a str> = body.split('\n').collect();
        if let Some(index) = lines.iter().position(|l| l.is_empty()) {
            return Err(CanonError::new(
                index.saturating_add(1),
                CanonErrorKind::EmptyLine,
            ));
        }
        let first = lines.first().and_then(|l| l.strip_prefix("scheme="));
        if first != Some(scheme) {
            return Err(CanonError::new(1, CanonErrorKind::WrongScheme));
        }
        Ok(Self { lines, pos: 1 })
    }

    /// An error at the line read last.
    pub(crate) fn err(&self, kind: CanonErrorKind) -> CanonError {
        CanonError::new(self.pos, kind)
    }

    /// The next line's value for `key`; `None` when the value is empty.
    pub(crate) fn take_opt(&mut self, key: &str) -> Result<Option<&'a str>, CanonError> {
        let number = self.pos.saturating_add(1);
        let Some(line) = self.lines.get(self.pos).copied() else {
            return Err(CanonError::new(number, CanonErrorKind::MissingLine));
        };
        let Some((found, value)) = line.split_once('=') else {
            return Err(CanonError::new(number, CanonErrorKind::MalformedLine));
        };
        if found != key {
            return Err(CanonError::new(number, CanonErrorKind::UnexpectedKey));
        }
        self.pos = number;
        Ok((!value.is_empty()).then_some(value))
    }

    pub(crate) fn take(&mut self, key: &str) -> Result<&'a str, CanonError> {
        match self.take_opt(key)? {
            Some(value) => Ok(value),
            None => Err(self.err(CanonErrorKind::EmptyValue)),
        }
    }

    /// A required value parsed by its grammar.
    pub(crate) fn value<T>(
        &mut self,
        key: &str,
        parse: impl FnOnce(&str) -> Result<T, ValueError>,
    ) -> Result<T, CanonError> {
        let raw = self.take(key)?;
        parse(raw).map_err(|e| self.err(CanonErrorKind::InvalidValue(e.kind())))
    }

    /// An optional value parsed by its grammar.
    pub(crate) fn opt_value<T>(
        &mut self,
        key: &str,
        parse: impl FnOnce(&str) -> Result<T, ValueError>,
    ) -> Result<Option<T>, CanonError> {
        match self.take_opt(key)? {
            None => Ok(None),
            Some(raw) => parse(raw)
                .map(Some)
                .map_err(|e| self.err(CanonErrorKind::InvalidValue(e.kind()))),
        }
    }

    /// `<path>.count`, which must lie in `min..=max`.
    pub(crate) fn count(
        &mut self,
        path: &str,
        min: usize,
        max: usize,
    ) -> Result<usize, CanonError> {
        let n = self.value(&format!("{path}.count"), values::parse_u64)?;
        match usize::try_from(n) {
            Ok(n) if (min..=max).contains(&n) => Ok(n),
            _ => Err(self.err(CanonErrorKind::CountOutOfRange)),
        }
    }

    pub(crate) fn finish(self) -> Result<(), CanonError> {
        if self.pos < self.lines.len() {
            return Err(CanonError::new(
                self.pos.saturating_add(1),
                CanonErrorKind::TrailingLine,
            ));
        }
        Ok(())
    }
}

/// Parses, requires every line consumed, re-encodes and requires identical bytes.
pub(crate) fn decode_exact<T>(
    bytes: &[u8],
    scheme: &str,
    max: usize,
    read: impl FnOnce(&mut Reader<'_>) -> Result<T, CanonError>,
    write: impl FnOnce(&T) -> Result<Vec<u8>, CanonError>,
) -> Result<T, CanonError> {
    let mut reader = Reader::new(bytes, scheme, max)?;
    let value = read(&mut reader)?;
    reader.finish()?;
    match write(&value) {
        Ok(again) if again == bytes => Ok(value),
        _ => Err(CanonError::new(0, CanonErrorKind::NonCanonical)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ValueKind;

    const SCHEME: &str = "silvana.agent.test.v1";

    #[derive(Debug, PartialEq)]
    struct Doc {
        name: String,
        note: Option<String>,
        items: Vec<u64>,
    }

    fn write(doc: &Doc) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME, 256);
        w.line("name", &doc.name);
        w.opt("note", doc.note.as_deref());
        w.count("items", doc.items.len());
        for (i, item) in doc.items.iter().enumerate() {
            w.line(&format!("items.{i}"), &item.to_string());
        }
        w.finish()
    }

    fn read(r: &mut Reader<'_>) -> Result<Doc, CanonError> {
        let name = r.take("name")?.to_owned();
        let note = r.take_opt("note")?.map(str::to_owned);
        let n = r.count("items", 0, 3)?;
        let mut items: Vec<u64> = Vec::new();
        for i in 0..n {
            let item = r.value(&format!("items.{i}"), values::parse_u64)?;
            if let Some(kind) = check_ascending(items.last(), &item) {
                return Err(r.err(kind));
            }
            items.push(item);
        }
        Ok(Doc { name, note, items })
    }

    fn decode(bytes: &[u8]) -> Result<Doc, CanonError> {
        decode_exact(bytes, SCHEME, 256, read, write)
    }

    fn sample() -> Doc {
        Doc {
            name: "a reasonably long name".to_owned(),
            note: None,
            items: vec![1, 5, 9],
        }
    }

    fn sample_text() -> String {
        String::from_utf8(write(&sample()).unwrap()).unwrap()
    }

    #[test]
    fn writes_the_expected_lines_and_round_trips() {
        let text = sample_text();
        assert_eq!(
            text,
            "scheme=silvana.agent.test.v1\nname=a reasonably long name\nnote=\nitems.count=3\n\
             items.0=1\nitems.1=5\nitems.2=9\n"
        );
        assert_eq!(decode(text.as_bytes()).unwrap(), sample());
    }

    #[test]
    fn writer_refuses_bad_values_and_sizes() {
        let mut doc = sample();
        doc.name = String::new();
        assert_eq!(write(&doc).unwrap_err().kind, CanonErrorKind::EmptyValue);
        doc.name = "line\nbreak in a long enough name".to_owned();
        assert_eq!(write(&doc).unwrap_err().kind, CanonErrorKind::InvalidByte);
        doc.name = "x".repeat(300);
        assert_eq!(write(&doc).unwrap_err().kind, CanonErrorKind::TooLarge);
        let mut w = Writer::new(SCHEME, 256);
        w.line("name", "x");
        assert_eq!(w.finish().unwrap_err().kind, CanonErrorKind::TooShort);
        let mut w = Writer::new(SCHEME, 256);
        w.line("Name", "a reasonably long name for the size");
        assert_eq!(w.finish().unwrap_err().kind, CanonErrorKind::MalformedLine);
    }

    #[test]
    fn reader_refusals_have_stable_names() {
        let good = sample_text();
        let cases: Vec<(&str, String, &str)> = vec![
            ("cr", good.replacen("name=", "name=\r", 1), "invalid_byte"),
            ("crlf", good.replace('\n', "\r\n"), "invalid_byte"),
            ("tab", good.replacen("long", "\tlong", 1), "invalid_byte"),
            ("nul", good.replacen("long", "\0long", 1), "invalid_byte"),
            ("bom", format!("\u{feff}{good}"), "invalid_byte"),
            (
                "non-ascii",
                good.replacen("long", "l\u{f6}ng", 1),
                "invalid_byte",
            ),
            (
                "no final lf",
                good.trim_end_matches('\n').to_owned(),
                "missing_final_newline",
            ),
            (
                "empty line",
                good.replacen("note=\n", "note=\n\n", 1),
                "empty_line",
            ),
            ("trailing line", format!("{good}extra=1\n"), "trailing_line"),
            (
                "duplicate line",
                good.replacen("note=\n", "note=\nnote=\n", 1),
                "unexpected_key",
            ),
            (
                "swapped lines",
                good.replacen(
                    "name=a reasonably long name\nnote=\n",
                    "note=\nname=a reasonably long name\n",
                    1,
                ),
                "unexpected_key",
            ),
            (
                "unknown key",
                good.replacen("note=", "notes=", 1),
                "unexpected_key",
            ),
            (
                "missing key",
                good.replacen("note=\n", "", 1),
                "unexpected_key",
            ),
            (
                "missing line",
                good.replacen("items.2=9\n", "", 1),
                "missing_line",
            ),
            (
                "no equals",
                good.replacen("note=", "note", 1),
                "malformed_line",
            ),
            (
                "wrong scheme",
                good.replacen("test.v1", "test.v2", 1),
                "wrong_scheme",
            ),
            (
                "empty required",
                good.replacen("name=a reasonably long name", "name=", 1),
                "empty_value",
            ),
            (
                "count leading zero",
                good.replacen("items.count=3", "items.count=03", 1),
                "invalid_u64",
            ),
            (
                "count too big",
                good.replacen("items.count=3", "items.count=4", 1),
                "count_out_of_range",
            ),
            (
                "count mismatch",
                good.replacen("items.count=3", "items.count=2", 1),
                "trailing_line",
            ),
            (
                "index out of order",
                good.replacen("items.1=5\nitems.2=9", "items.2=9\nitems.1=5", 1),
                "unexpected_key",
            ),
            (
                "unsorted",
                good.replacen("items.1=5\nitems.2=9", "items.1=9\nitems.2=5", 1),
                "unsorted_list",
            ),
            (
                "duplicate item",
                good.replacen("items.2=9", "items.2=5", 1),
                "duplicate_item",
            ),
            (
                "too short",
                "scheme=silvana.agent.test.v1\nname=x\n".to_owned(),
                "too_short",
            ),
            (
                "too large",
                good.replacen("long", &"o".repeat(300), 1),
                "too_large",
            ),
        ];
        for (name, text, expected) in cases {
            let err = decode(text.as_bytes()).unwrap_err();
            assert_eq!(err.code_name(), expected, "{name}");
        }
    }

    #[test]
    fn value_errors_point_at_their_line() {
        let text = sample_text().replacen("items.1=5", "items.1=+5", 1);
        let err = decode(text.as_bytes()).unwrap_err();
        assert_eq!(err.kind, CanonErrorKind::InvalidValue(ValueKind::U64));
        assert_eq!(err.line, 6);
    }

    #[test]
    fn a_value_that_re_encodes_differently_is_non_canonical() {
        let read_lossy = |r: &mut Reader<'_>| -> Result<Doc, CanonError> {
            let mut doc = read(r)?;
            doc.name = doc.name.to_uppercase();
            Ok(doc)
        };
        let text = sample_text();
        let err = decode_exact(text.as_bytes(), SCHEME, 256, read_lossy, write).unwrap_err();
        assert_eq!(err.kind, CanonErrorKind::NonCanonical);
    }
}
