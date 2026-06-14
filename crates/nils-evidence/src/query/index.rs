//! Stamp codec helpers shared by the query layer.
//!
//! Evidence has **no** `_index/` provider-snapshot tree (skill-usage rollups
//! carry no provider refs), so — unlike plan-archive — there is no
//! `IndexEntry`/`walk_index`/`parse_index_path` here. What remains is the
//! basic-stamp codec used to render/parse the `YYYYMMDDThhmmssZ` stamp that
//! prefixes every rollup id.

/// Decode a basic-format `YYYYMMDDThhmmssZ` stamp back into an extended
/// ISO8601 `YYYY-MM-DDThh:mm:ssZ`. Returns `None` when the input does not
/// match the expected shape, honoring the `Option` contract (and matching the
/// migrate-side `encode_basic_stamp` counterpart, which only round-trips a
/// well-formed stamp). (Inverse of [`crate::migrate::encode_basic_stamp`].)
pub fn decode_basic_stamp(stamp: &str) -> Option<String> {
    let bytes = stamp.as_bytes();
    if bytes.len() == 16 && bytes[8] == b'T' && bytes[15] == b'Z' {
        let digits =
            |range: std::ops::Range<usize>| stamp[range].chars().all(|c| c.is_ascii_digit());
        if digits(0..8) && digits(9..15) {
            return Some(format!(
                "{}-{}-{}T{}:{}:{}Z",
                &stamp[0..4],
                &stamp[4..6],
                &stamp[6..8],
                &stamp[9..11],
                &stamp[11..13],
                &stamp[13..15],
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::encode_basic_stamp;

    #[test]
    fn decode_extended() {
        assert_eq!(
            decode_basic_stamp("20260527T013045Z").as_deref(),
            Some("2026-05-27T01:30:45Z")
        );
    }

    #[test]
    fn decode_returns_none_on_malformed_input() {
        // F10: malformed input must honor the `Option` contract and return
        // `None`, not `Some(raw)`.
        assert_eq!(decode_basic_stamp("weird"), None);
        // Right length but wrong separators / non-digits.
        assert_eq!(decode_basic_stamp("2026-06-14T10:00"), None);
        assert_eq!(decode_basic_stamp("20260614X100000Z"), None);
        assert_eq!(decode_basic_stamp("2026061aT100000Z"), None);
    }

    #[test]
    fn encode_decode_round_trips() {
        let rfc = "2026-06-14T10:20:30Z";
        let encoded = encode_basic_stamp(rfc);
        assert_eq!(decode_basic_stamp(&encoded).as_deref(), Some(rfc));
    }
}
