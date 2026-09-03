// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Base64 decoding for CherryTree's `<encoded_png>` payloads.
//!
//! Standard alphabet, `=` padding, and whitespace ignored, because
//! CherryTree wraps long payloads across lines. Any other character is a
//! refusal rather than a guess: a mis-decoded image is a corrupt file that
//! looks like a successful import.

/// Decode standard base64, ignoring ASCII whitespace.
pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut padding = 0usize;

    for ch in s.chars() {
        if ch.is_ascii_whitespace() {
            continue;
        }
        if ch == '=' {
            padding += 1;
            continue;
        }
        if padding > 0 {
            return Err("base64 data after padding".to_string());
        }
        let v = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return Err(format!("invalid base64 character {ch:?}")),
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }

    if padding > 2 {
        return Err("more than two base64 padding characters".to_string());
    }
    // Leftover bits must be zero; anything else means a truncated payload.
    if acc != 0 {
        return Err("base64 payload has non-zero trailing bits".to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        for (encoded, plain) in [
            ("", ""),
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("Zm9vYg==", "foob"),
            ("Zm9vYmE=", "fooba"),
            ("Zm9vYmFy", "foobar"),
        ] {
            assert_eq!(decode(encoded).unwrap(), plain.as_bytes(), "{encoded}");
        }
    }

    #[test]
    fn whitespace_between_lines_is_ignored() {
        assert_eq!(decode("Zm9v\n  Ym Fy\n").unwrap(), b"foobar");
    }

    #[test]
    fn a_png_signature_survives_a_round_trip() {
        // The eight-byte PNG signature, which is what a real payload starts
        // with. High bytes exercise the non-ASCII path.
        assert_eq!(
            decode("iVBORw0KGgo=").unwrap(),
            [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    #[test]
    fn invalid_input_is_refused_not_guessed() {
        assert!(decode("Zm9v!").is_err());
        assert!(decode("Zm9v=Zg==").is_err());
        assert!(decode("Zg===").is_err());
    }
}
