//! Turning file bytes into document text.

use std::borrow::Cow;
use std::fmt;

const BOM: &[u8] = b"\xEF\xBB\xBF";
/// A NUL byte within this prefix marks the file as binary.
const BINARY_PROBE: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded<'a> {
    /// Document text without the BOM, borrowed from the bytes when they are valid UTF-8.
    /// For invalid UTF-8, invalid bytes become U+FFFD.
    pub text: Cow<'a, str>,
    /// The file started with a UTF-8 BOM; saving writes it back.
    pub bom: bool,
    /// `Some` when the document must open read-only.
    pub read_only: Option<ReadOnlyReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyReason {
    InvalidUtf8,
}

impl fmt::Display for ReadOnlyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 => {
                f.write_str("Editing is disabled because the file is not valid UTF-8.")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// A NUL byte appeared in the first 8 KiB.
    Binary,
}

pub fn decode(bytes: &[u8]) -> Result<Decoded<'_>, DecodeError> {
    if memchr::memchr(0, &bytes[..bytes.len().min(BINARY_PROBE)]).is_some() {
        return Err(DecodeError::Binary);
    }
    let (bom, body) = match bytes.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, bytes),
    };
    Ok(match std::str::from_utf8(body) {
        Ok(text) => Decoded {
            text: Cow::Borrowed(text),
            bom,
            read_only: None,
        },
        Err(_) => Decoded {
            text: String::from_utf8_lossy(body),
            bom,
            read_only: Some(ReadOnlyReason::InvalidUtf8),
        },
    })
}

/// The bytes to write for `text`: the BOM (if the file had one) followed by the text verbatim.
pub fn encode(text: impl IntoIterator<Item = impl AsRef<[u8]>>, bom: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if bom {
        out.extend_from_slice(BOM);
    }
    for chunk in text {
        out.extend_from_slice(chunk.as_ref());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_utf8() {
        let d = decode("# Title\né\n".as_bytes()).unwrap();
        assert_eq!(
            d,
            Decoded {
                text: "# Title\né\n".into(),
                bom: false,
                read_only: None
            }
        );
    }

    #[test]
    fn bom_is_stripped_and_remembered_and_written_back() {
        let bytes = b"\xEF\xBB\xBF# Hi\r\n";
        let d = decode(bytes).unwrap();
        assert_eq!(d.text, "# Hi\r\n");
        assert!(d.bom);
        assert_eq!(encode([d.text.as_bytes()], d.bom), bytes);
    }

    #[test]
    fn invalid_utf8_opens_read_only_with_replacement_characters() {
        let d = decode(b"ok \xFF\xFE bad").unwrap();
        assert_eq!(d.text, "ok \u{FFFD}\u{FFFD} bad");
        assert_eq!(d.read_only, Some(ReadOnlyReason::InvalidUtf8));
        assert!(d.read_only.unwrap().to_string().contains("not valid UTF-8"));
    }

    #[test]
    fn nul_in_first_8_kib_is_binary() {
        let mut bytes = vec![b'a'; 8 * 1024];
        bytes[8 * 1024 - 1] = 0;
        assert_eq!(decode(&bytes), Err(DecodeError::Binary));
    }

    #[test]
    fn nul_after_8_kib_is_not_binary() {
        let mut bytes = vec![b'a'; 8 * 1024];
        bytes.push(0);
        assert!(decode(&bytes).is_ok());
    }

    #[test]
    fn empty_file() {
        assert_eq!(decode(b"").unwrap().text, "");
    }
}
