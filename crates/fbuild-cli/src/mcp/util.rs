//! Small standalone helpers for the MCP server (URI decoding, UUID generation).

/// Simple percent-decoding for URI path segments.
pub(super) fn urlencoding_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().unwrap_or(b'0');
            let lo = chars.next().unwrap_or(b'0');
            let val = hex_val(hi) * 16 + hex_val(lo);
            result.push(val as char);
        } else {
            result.push(b as char);
        }
    }
    result
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// Generate a UUID v4 string (simple implementation without extra deps).
pub(super) fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Use time-based pseudo-random (good enough for request IDs, not crypto)
    let pid = std::process::id() as u128;
    let val = seed ^ (pid << 32) ^ (seed >> 16);
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (val >> 96) as u32,
        (val >> 80) as u16,
        (val >> 64) as u16 & 0x0fff,
        ((val >> 48) as u16 & 0x3fff) | 0x8000,
        val as u64 & 0xffffffffffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoding_decode_works() {
        assert_eq!(urlencoding_decode("hello%20world"), "hello world");
        assert_eq!(urlencoding_decode("C%3A%5Cdev"), "C:\\dev");
        assert_eq!(urlencoding_decode("no-encoding"), "no-encoding");
    }

    #[test]
    fn uuid_v4_has_correct_format() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().nth(8), Some('-'));
        assert_eq!(id.chars().nth(13), Some('-'));
        assert_eq!(id.chars().nth(14), Some('4')); // version 4
        assert_eq!(id.chars().nth(18), Some('-'));
    }
}
