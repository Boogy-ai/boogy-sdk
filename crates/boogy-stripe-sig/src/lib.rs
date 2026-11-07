//! Parser for the `Stripe-Signature` webhook header.
//!
//! This crate only parses and applies the replay window; it never computes or
//! compares a MAC. The caller verifies each returned `v1` value against
//! [`SignedPayload::message`] with its own HMAC-SHA256 implementation and
//! accepts the event if any of them verifies. Stripe sends one `v1` per active
//! signing secret, so during a secret rotation the header carries two.

/// Replay tolerance Stripe recommends, in seconds.
pub const DEFAULT_TOLERANCE_S: i64 = 300;

/// The most `v1` values accepted. Stripe sends one per active signing secret
/// (two during a rotation); a header with more is refused before any of them
/// is verified, so one request cannot fan out into many verifications.
pub const MAX_V1_SIGNATURES: usize = 4;

/// A header that passed parsing and the replay window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedPayload {
    /// The `t=` timestamp, in seconds since the epoch.
    pub timestamp: i64,
    /// The signed message: `b"{t}."` followed by the raw body bytes.
    pub message: Vec<u8>,
    /// Every non-empty `v1=` value, in header order.
    pub v1: Vec<String>,
}

/// Why a header was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigError {
    /// A `t=` element was present but is not an integer.
    Malformed,
    /// No `t=` element.
    NoTimestamp,
    /// No non-empty `v1=` element.
    NoV1,
    /// The timestamp is further from `now_s` than the tolerance, in either direction.
    Stale,
    /// More than [`MAX_V1_SIGNATURES`] non-empty `v1=` elements.
    TooManySignatures,
}

/// Parse `header`, enforce the replay window, and build the signed message.
///
/// Total: every input yields `Ok` or an `Err`, never a panic. Unknown keys
/// (such as `v0`) are ignored, and an empty `v1=` is skipped so an empty
/// expected signature can never be offered for verification.
pub fn parse(
    header: &str,
    body: &[u8],
    now_s: i64,
    tolerance_s: i64,
) -> Result<SignedPayload, SigError> {
    let mut t: Option<i64> = None;
    let mut malformed = false;
    let mut v1: Vec<String> = Vec::new();
    for elem in header.split(',') {
        let Some((key, value)) = elem.trim().split_once('=') else {
            continue;
        };
        match key {
            "t" if t.is_none() => match value.parse::<i64>() {
                Ok(n) => t = Some(n),
                Err(_) => malformed = true,
            },
            "v1" if !value.is_empty() => {
                if v1.len() == MAX_V1_SIGNATURES {
                    return Err(SigError::TooManySignatures);
                }
                v1.push(value.to_string());
            }
            _ => {}
        }
    }

    let t = match t {
        Some(t) => t,
        None if malformed => return Err(SigError::Malformed),
        None => return Err(SigError::NoTimestamp),
    };
    if v1.is_empty() {
        return Err(SigError::NoV1);
    }
    // Absolute skew refuses both stale and clock-skewed-forward timestamps.
    // Saturating arithmetic keeps extreme inputs from overflowing.
    if now_s.saturating_sub(t).saturating_abs() > tolerance_s {
        return Err(SigError::Stale);
    }

    let mut message = format!("{t}.").into_bytes();
    message.extend_from_slice(body);
    Ok(SignedPayload { timestamp: t, message, v1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    #[test]
    fn two_v1_signatures_are_both_returned() {
        let p = parse("t=1700000000,v1=aa,v0=zz,v1=bb", b"{}", NOW, 300).unwrap();
        assert_eq!(p.v1, ["aa", "bb"]);
        assert_eq!(p.timestamp, NOW);
    }

    #[test]
    fn more_than_max_v1_is_refused() {
        let h = |n: usize| {
            let v1: Vec<String> = (0..n).map(|i| format!("v1=s{i}")).collect();
            format!("t=1,{}", v1.join(","))
        };
        // Literal counts, so changing the cap makes this test fail.
        let ok = parse(&h(4), b"{}", 1, 300).unwrap();
        assert_eq!(ok.v1.len(), 4);
        assert_eq!(parse(&h(5), b"{}", 1, 300), Err(SigError::TooManySignatures));
    }

    #[test]
    fn empty_v1_is_skipped() {
        let p = parse("t=1,v1=,v1=cc", b"{}", 1, 300).unwrap();
        assert_eq!(p.v1, ["cc"]);
    }

    #[test]
    fn no_v1_is_an_error() {
        assert_eq!(parse("t=1", b"{}", 1, 300), Err(SigError::NoV1));
        assert_eq!(parse("t=1,v1=,v0=aa", b"{}", 1, 300), Err(SigError::NoV1));
    }

    #[test]
    fn stale_and_future_timestamps_refused() {
        let h = |t: i64| format!("t={t},v1=aa");
        assert_eq!(parse(&h(NOW - 301), b"", NOW, 300), Err(SigError::Stale));
        assert_eq!(parse(&h(NOW + 301), b"", NOW, 300), Err(SigError::Stale));
        assert!(parse(&h(NOW - 300), b"", NOW, 300).is_ok());
        assert!(parse(&h(NOW + 300), b"", NOW, 300).is_ok());
    }

    #[test]
    fn message_is_t_dot_body() {
        let p = parse("t=1700000000,v1=aa", b"{\"a\":1}", NOW, 300).unwrap();
        assert_eq!(p.message, b"1700000000.{\"a\":1}");
    }

    #[test]
    fn message_preserves_non_utf8_body() {
        let body = [0x7b, 0xff, 0x7d];
        let p = parse("t=1000,v1=abc", &body, 1000, 300).unwrap();
        let mut want = b"1000.".to_vec();
        want.extend_from_slice(&body);
        assert_eq!(p.message, want);
    }

    #[test]
    fn garbage_header_is_malformed() {
        for h in ["", "t=abc,v1=aa", "v1=aa", "garbage", "t=notanumber,v1=abc"] {
            let e = parse(h, b"{}", NOW, 300).unwrap_err();
            assert!(matches!(e, SigError::Malformed | SigError::NoTimestamp), "{h:?} -> {e:?}");
        }
    }

    #[test]
    fn tolerance_zero_requires_exact() {
        assert!(parse("t=1000,v1=abc", b"{}", 1000, 0).is_ok());
        assert_eq!(parse("t=1001,v1=abc", b"{}", 1000, 0), Err(SigError::Stale));
        assert_eq!(parse("t=999,v1=abc", b"{}", 1000, 0), Err(SigError::Stale));
    }

    #[test]
    fn extreme_timestamps_do_not_panic() {
        let lo = format!("t={},v1=a", i64::MIN);
        let hi = format!("t={},v1=a", i64::MAX);
        assert_eq!(parse(&lo, b"", i64::MAX, 300), Err(SigError::Stale));
        assert_eq!(parse(&hi, b"", i64::MIN, 300), Err(SigError::Stale));
    }
}
