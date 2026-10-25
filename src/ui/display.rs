//! Display-only transformations of cell values. Stored values never change.

use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BinaryDisplay {
    #[default]
    Hex,
    Base64,
    Ulid,
}

impl BinaryDisplay {
    pub const ALL: [Self; 3] = [Self::Hex, Self::Base64, Self::Ulid];

    pub fn label(self) -> &'static str {
        match self {
            Self::Hex => "Hex",
            Self::Base64 => "Base64",
            Self::Ulid => "ULID",
        }
    }
}

/// Shows a `bytea` value (PostgreSQL hex output such as `\x00ff`) in the chosen
/// form. ULID applies only to 16-byte values; others stay hex.
pub fn binary(text: &str, mode: BinaryDisplay) -> Cow<'_, str> {
    if mode == BinaryDisplay::Hex {
        return Cow::Borrowed(text);
    }
    let Some(bytes) = decode_hex(text) else {
        return Cow::Borrowed(text);
    };
    match mode {
        BinaryDisplay::Base64 => Cow::Owned(base64(&bytes)),
        BinaryDisplay::Ulid => ulid(&bytes).map_or(Cow::Borrowed(text), Cow::Owned),
        BinaryDisplay::Hex => Cow::Borrowed(text),
    }
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let digits = text.strip_prefix("\\x")?;
    if digits.len() % 2 != 0 {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(digits.get(index..index + 2)?, 16).ok())
        .collect()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for position in 0..4 {
            if position <= chunk.len() {
                out.push(ALPHABET[(value >> (18 - 6 * position) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Crockford base32 encoding of a 128-bit value, as used by ULID.
fn ulid(bytes: &[u8]) -> Option<String> {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let value = u128::from_be_bytes(bytes.try_into().ok()?);
    Some(
        (0..26)
            .map(|index| ALPHABET[((value >> (125 - 5 * index)) & 31) as usize] as char)
            .collect(),
    )
}

/// Re-indents JSON text without parsing values, so key order and exact numbers are
/// kept. Returns `None` when the text is not valid JSON.
pub fn pretty_json(text: &str) -> Option<String> {
    serde_json::from_str::<serde::de::IgnoredAny>(text).ok()?;
    let mut out = String::with_capacity(text.len() + text.len() / 2);
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = text.chars().peekable();
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                while chars.next_if(|c| c.is_whitespace()).is_some() {}
                if matches!(chars.peek(), Some('}' | ']')) {
                    out.push(chars.next().expect("peeked"));
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_values_display_as_base64_and_ulid() {
        assert_eq!(binary("\\x00ff10", BinaryDisplay::Base64), "AP8Q");
        assert_eq!(binary("\\x4d616e", BinaryDisplay::Base64), "TWFu");
        assert_eq!(binary("\\x4d61", BinaryDisplay::Base64), "TWE=");
        assert_eq!(binary("\\x", BinaryDisplay::Base64), "");
        let ulid_bytes = "\\x01563df36d0fd2e5bd8b9c4f0b0e6e1a";
        assert_eq!(binary(ulid_bytes, BinaryDisplay::Ulid).len(), 26);
        assert_eq!(
            binary("\\x00000000000000000000000000000000", BinaryDisplay::Ulid),
            "00000000000000000000000000"
        );
        assert_eq!(
            binary("\\xffffffffffffffffffffffffffffffff", BinaryDisplay::Ulid),
            "7ZZZZZZZZZZZZZZZZZZZZZZZZZ"
        );
        assert_eq!(binary("\\x00ff", BinaryDisplay::Ulid), "\\x00ff");
        assert_eq!(binary("not hex", BinaryDisplay::Base64), "not hex");
    }

    #[test]
    fn json_is_indented_without_reordering_or_rounding() {
        let pretty =
            pretty_json(r#"{"z":1,"a":[12345678901234567890.5,{}],"s":"x, {y}: \"q\""}"#).unwrap();
        assert_eq!(
            pretty,
            "{\n  \"z\": 1,\n  \"a\": [\n    12345678901234567890.5,\n    {}\n  ],\n  \"s\": \"x, {y}: \\\"q\\\"\"\n}"
        );
        assert!(pretty_json("{oops").is_none());
    }
}
