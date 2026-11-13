//! Currently only support utf8 mode

#![no_std]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

#[cfg(feature = "std")]
use std::io::{self, Write};

const UTF8_START: &[u8] = &[0x1B, 0x25, 0x47];
const UTF8_END: &[u8] = &[0x1B, 0x25, 0x40];

/// Wrapper for reduce allocation
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct CText<'s> {
    utf8: &'s str,
}

impl<'s> fmt::Debug for CText<'s> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.utf8)
    }
}

impl<'s> fmt::Display for CText<'s> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.utf8)
    }
}

impl<'s> CText<'s> {
    pub const fn new(utf8: &'s str) -> Self {
        Self { utf8 }
    }

    pub const fn len(self) -> usize {
        self.utf8.len() + UTF8_START.len() + UTF8_END.len()
    }

    #[cfg(feature = "std")]
    pub fn write(self, mut out: impl Write) -> io::Result<usize> {
        let mut writed = 0;
        writed += out.write(UTF8_START)?;
        writed += out.write(self.utf8.as_bytes())?;
        writed += out.write(UTF8_END)?;
        Ok(writed)
    }
}

/// Encoding utf8 to COMPOUND_TEXT with utf8 escape
pub fn utf8_to_compound_text(text: &str) -> Vec<u8> {
    let mut ret = Vec::with_capacity(text.len() + 6);
    ret.extend_from_slice(UTF8_START);
    ret.extend_from_slice(text.as_bytes());
    ret.extend_from_slice(UTF8_END);
    ret
}

#[derive(Debug, Clone)]
pub enum DecodeError {
    InvalidEncoding,
    UnsupportedEncoding,
    Utf8Error(alloc::string::FromUtf8Error),
}

impl From<alloc::string::FromUtf8Error> for DecodeError {
    fn from(err: alloc::string::FromUtf8Error) -> Self {
        DecodeError::Utf8Error(err)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEncoding => write!(f, "Invalid compound text"),
            Self::UnsupportedEncoding => write!(f, "This encoding is not supported yet"),
            Self::Utf8Error(e) => write!(f, "Not a valid utf8 {}", e),
        }
    }
}

// md-tex: the upstream decoder handled only UTF-8 segments and a single JIS X 0208
// segment, and GPUI's XIM client panics on any error. IBus sends a dead key's preview (`´`)
// as unescaped Latin-1 and simplified Chinese as GB 2312, so both crashed the app on X11.
// This decodes every charset Xlib writes into COMPOUND_TEXT and never fails: unknown sets
// and malformed bytes become U+FFFD.

#[derive(Clone, Copy)]
enum Set {
    Ascii,
    JisRoman,
    Katakana,
    /// The right half of an ISO 8859 part; `None` is Latin-1.
    Latin(Option<&'static encoding_rs::Encoding>),
    /// A 94x94 set, decoded as its EUC form.
    Double(&'static encoding_rs::Encoding),
    Unknown,
    UnknownDouble,
}

impl Set {
    fn width(self) -> usize {
        match self {
            Set::Double(_) | Set::UnknownDouble => 2,
            _ => 1,
        }
    }

    fn push(self, bytes: &[u8], out: &mut String) {
        let gr = |b: u8| b | 0x80;
        match self {
            Set::Ascii => out.push(char::from(bytes[0] & 0x7F)),
            Set::JisRoman => out.push(match bytes[0] & 0x7F {
                0x5C => '\u{A5}',
                0x7E => '\u{203E}',
                b => char::from(b),
            }),
            Set::Katakana => match bytes[0] & 0x7F {
                b @ 0x21..=0x5F => out.push(char::from_u32(0xFF61 + u32::from(b - 0x21)).unwrap()),
                _ => out.push(char::REPLACEMENT_CHARACTER),
            },
            Set::Latin(None) => out.push(char::from(gr(bytes[0]))),
            Set::Latin(Some(enc)) => out.push_str(&enc.decode_without_bom_handling(&[gr(bytes[0])]).0),
            Set::Double(enc) => {
                out.push_str(&enc.decode_without_bom_handling(&[gr(bytes[0]), gr(bytes[1])]).0)
            }
            Set::Unknown | Set::UnknownDouble => out.push(char::REPLACEMENT_CHARACTER),
        }
    }
}

fn single(f: u8) -> Set {
    match f {
        b'B' => Set::Ascii,
        b'J' => Set::JisRoman,
        b'I' => Set::Katakana,
        _ => Set::Unknown,
    }
}

fn latin(f: u8) -> Set {
    use encoding_rs::*;
    Set::Latin(Some(match f {
        b'A' => return Set::Latin(None),
        b'B' => ISO_8859_2,
        b'C' => ISO_8859_3,
        b'D' => ISO_8859_4,
        b'F' => ISO_8859_7,
        b'G' => ISO_8859_6,
        b'H' => ISO_8859_8,
        b'L' => ISO_8859_5,
        // ISO 8859-9; windows-1254 differs from it only below 0xA0.
        b'M' => WINDOWS_1254,
        b'Y' => ISO_8859_13,
        b'_' => ISO_8859_14,
        b'b' => ISO_8859_15,
        b'f' => ISO_8859_16,
        _ => return Set::Unknown,
    }))
}

fn double(f: u8) -> Set {
    match f {
        b'A' => Set::Double(encoding_rs::GBK),
        b'B' => Set::Double(encoding_rs::EUC_JP),
        b'C' => Set::Double(encoding_rs::EUC_KR),
        _ => Set::UnknownDouble,
    }
}

pub fn compound_text_to_utf8(bytes: &[u8]) -> Result<String, DecodeError> {
    // Some input methods send plain UTF-8 without the escape, which upstream accepted.
    if !bytes.contains(&0x1B) {
        if let Ok(s) = core::str::from_utf8(bytes) {
            return Ok(String::from(s));
        }
    }
    let (mut gl, mut gr) = (Set::Ascii, Set::Latin(None));
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1B {
            let at = |k: usize| bytes.get(i + k).copied().unwrap_or(0);
            i += match (at(1), at(2), at(3)) {
                (b'(', f, _) => {
                    gl = single(f);
                    3
                }
                (b')', f, _) => {
                    gr = single(f);
                    3
                }
                (b'-', f, _) => {
                    gr = latin(f);
                    3
                }
                (b'$', b'(', f) => {
                    gl = double(f);
                    4
                }
                (b'$', b')', f) => {
                    gr = double(f);
                    4
                }
                (b'%', b'G', _) => {
                    let rest = &bytes[(i + 3).min(bytes.len())..];
                    let len = rest
                        .windows(3)
                        .position(|w| w == UTF8_END)
                        .unwrap_or(rest.len());
                    out.push_str(&String::from_utf8_lossy(&rest[..len]));
                    3 + len + 3
                }
                // Extended segments (ESC % / F M L) carry their own length.
                (b'%', b'/', _) => {
                    out.push(char::REPLACEMENT_CHARACTER);
                    6 + (usize::from(at(4) & 0x7F) << 7 | usize::from(at(5) & 0x7F))
                }
                _ => {
                    out.push(char::REPLACEMENT_CHARACTER);
                    1
                }
            };
        } else if b == 0x9B {
            // A direction sequence (CSI ... F): there is nothing to draw.
            i += 1;
            while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                i += 1;
            }
            i += 1;
        } else if b < 0x20 || b == 0x7F {
            out.push(char::from(b));
            i += 1;
        } else if (0x80..0xA0).contains(&b) {
            i += 1;
        } else {
            let set = if b < 0x80 { gl } else { gr };
            match bytes.get(i..i + set.width()) {
                Some(c) if c.iter().all(|&c| (c >= 0x80) == (b >= 0x80)) => set.push(c, &mut out),
                _ => out.push(char::REPLACEMENT_CHARACTER),
            }
            i += set.width();
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn korean() {
        const UTF8: &str = "가나다";
        const COMP: &[u8] = &[
            27, 37, 71, 234, 176, 128, 235, 130, 152, 235, 139, 164, 27, 37, 64,
        ];
        assert_eq!(crate::utf8_to_compound_text(UTF8), COMP);
        assert_eq!(crate::compound_text_to_utf8(COMP).unwrap(), UTF8);
    }

    #[test]
    fn unescaped_latin1() {
        assert_eq!(crate::compound_text_to_utf8(&[0xB4]).unwrap(), "\u{B4}");
        assert_eq!(crate::compound_text_to_utf8(b"caf\xE9").unwrap(), "caf\u{E9}");
    }

    #[test]
    fn gb2312() {
        // 我们, as IBus sends it from a UTF-8 locale, in both halves.
        let gr = [0x1B, 0x24, 0x29, 0x41, 0xCE, 0xD2, 0xC3, 0xC7];
        let gl = [0x1B, 0x24, 0x28, 0x41, 0x4E, 0x52, 0x43, 0x47];
        assert_eq!(crate::compound_text_to_utf8(&gr).unwrap(), "我们");
        assert_eq!(crate::compound_text_to_utf8(&gl).unwrap(), "我们");
    }

    #[test]
    fn mixed_segments() {
        let mut ct = b"a\x1B$)C\xB0\xA1\x1B-L\xB0 b".to_vec();
        ct.extend_from_slice(&[0x1B, 0x25, 0x47, 0xE2, 0x82, 0xAC, 0x1B, 0x25, 0x40, b'!']);
        assert_eq!(crate::compound_text_to_utf8(&ct).unwrap(), "a가\u{410} b€!");
    }

    #[test]
    fn malformed_input_does_not_fail() {
        for ct in [
            &b"\x1B"[..],
            b"\x1B$",
            b"\x1B%G",
            b"\x1B$)A\xCE",
            b"\x1B$)Z\xCE\xD2",
            b"\x1B%/1\x7F\x7F",
            b"\x9B1",
            b"\xFF\xFE",
        ] {
            assert!(crate::compound_text_to_utf8(ct).is_ok());
        }
    }

    #[test]
    fn iso_2011_jp() {
        const UTF8: &str = "東京";
        const COMP: &[u8] = &[27, 36, 40, 66, 69, 108, 53, 126];
        assert_eq!(crate::compound_text_to_utf8(COMP).unwrap(), UTF8);
    }
}
