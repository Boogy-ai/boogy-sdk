//! Random values — ids, slugs, tokens, samples, shuffles, UUIDs.
//!
//! The platform exposes exactly one entropy primitive to a service:
//! `random-bytes(len) -> list<u8>`. Everything a service actually wants —
//! "a 6-character slug", "a number between 1 and 6", "one of these three
//! options", "a v4 UUID" — is arithmetic on top of those bytes, and that
//! arithmetic is easy to get subtly wrong (modulo bias, off-by-one range
//! ends, an alphabet that differs per service). This module does it once.
//!
//! # Two layers
//!
//! - [`Rng`] — the pure core. Generic over a [`ByteSource`], holds all the
//!   arithmetic, and has no platform imports at all, so it can be unit
//!   tested on the host with a deterministic (or adversarial) byte source.
//! - Free functions emitted by [`wit_glue!`](crate::wit_glue) into the
//!   service crate — `random_int`, `random_string`, `random_id`,
//!   `random_uuid_v4`, … — which are one-line wrappers that feed the host
//!   entropy source into an [`Rng`]. This is what handler code calls:
//!
//! ```ignore
//! let die = random_int(1, 6);                       // inclusive both ends
//! let slug = random_string(6, &Alphabet::HEX);      // "9f3a1c"
//! let public_id = random_id();                      // 22 url-safe chars
//! let request_id = random_uuid_v4();
//! let winner = random_choose(&candidates);          // Option<&T>
//! ```
//!
//! Reach for [`Rng`] directly (via the emitted `rng()`) when you want
//! several values from one handle or a method this module exposes but the
//! flat wrappers don't.
//!
//! # Capability
//!
//! Every function here consumes platform entropy, which requires the
//! `entropy` capability:
//!
//! ```toml
//! [capabilities]
//! entropy = true
//! ```
//!
//! Without the grant the host returns **zero bytes** rather than failing,
//! so values stay in range but stop being random (`random_int(1, 6)`
//! always returns `1`). If ids look constant in a deployment, check the
//! manifest first. The host also caps how many bytes one call may return;
//! [`Rng::bytes`] zero-pads a short read so lengths are always exact.
//!
//! # Randomness quality
//!
//! The bytes come from the operating system's cryptographically secure
//! random number generator (the same source used to mint platform
//! credentials), so values from this module are safe for public ids,
//! tokens, and nonces. Two caveats that are properties of *this* module
//! rather than the source:
//!
//! - The mapping from bytes to values is **rejection-sampled**, so it
//!   adds no modulo bias. A value drawn from `random_int(0, 199)` is
//!   exactly as likely to be `0` as `199`.
//! - Entropy per value is bounded by the value space, not the algorithm:
//!   a 6-character hex slug is 24 bits and *will* collide. Size the
//!   output for the use (see [`Rng::id`], ~131 bits) and keep a unique
//!   index on any column that must not collide.
//!
//! # Ranges and totality
//!
//! Integer and float range helpers are **inclusive of both ends**
//! (`random_int(1, 6)` can return `6`) unless the name says `exclusive`.
//! Every range helper comes in two forms:
//!
//! - `try_*` returns `Result<_, RandomError>` and never panics.
//! - The short name panics on an empty range (`min > max`), matching the
//!   convention of range samplers elsewhere in the ecosystem. A panic in
//!   a service traps the guest and fails the request, so prefer the
//!   `try_*` form whenever the bounds are computed from input.
//!
//! Nothing here silently wraps, clamps, or swaps reversed bounds.

use std::fmt;

/// A source of raw random bytes.
///
/// Implemented blanket-style for every `FnMut(usize) -> Vec<u8>`, so a
/// closure (or a plain `fn` pointer, which is what the generated glue
/// hands over) is a valid source with no wrapper type:
///
/// ```
/// use boogy_sdk::random::Rng;
/// // A fixed, non-random source — the shape tests use.
/// let mut rng = Rng::new(|n: usize| vec![7u8; n]);
/// assert_eq!(rng.bytes(3), vec![7, 7, 7]);
/// ```
///
/// A source may return fewer bytes than asked for; [`Rng`] zero-pads.
/// It may not assume it is called with any particular length.
pub trait ByteSource {
    /// Produce up to `n` random bytes. Returning fewer is allowed;
    /// returning more is allowed and the extra is discarded.
    fn next_bytes(&mut self, n: usize) -> Vec<u8>;
}

impl<F> ByteSource for F
where
    F: FnMut(usize) -> Vec<u8>,
{
    fn next_bytes(&mut self, n: usize) -> Vec<u8> {
        self(n)
    }
}

/// Why a random-value request could not be served.
///
/// Every variant is a caller mistake (an empty range, a malformed
/// alphabet) rather than a transient failure — retrying an identical
/// call always fails identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RandomError {
    /// `min > max`, or an exclusive end at or below the start: there is
    /// no value the call could return.
    EmptyRange,
    /// The alphabet has no symbols.
    EmptyAlphabet,
    /// The alphabet contains a non-ASCII byte. Symbols are indexed
    /// bytewise, so multi-byte characters cannot be selected uniformly.
    NonAsciiAlphabet,
    /// The alphabet has more than 256 symbols.
    AlphabetTooLarge,
    /// A float bound is NaN or infinite, or the span between the bounds
    /// overflows to infinity.
    NonFiniteBound,
}

impl fmt::Display for RandomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RandomError::EmptyRange => write!(f, "empty range: min is greater than max"),
            RandomError::EmptyAlphabet => write!(f, "alphabet is empty"),
            RandomError::NonAsciiAlphabet => write!(f, "alphabet contains a non-ASCII byte"),
            RandomError::AlphabetTooLarge => write!(f, "alphabet has more than 256 symbols"),
            RandomError::NonFiniteBound => write!(f, "float bound is not finite"),
        }
    }
}

impl std::error::Error for RandomError {}

impl From<RandomError> for crate::error::ApiError {
    /// A random-value error is a programming mistake in the service, not
    /// something the client did, so it maps to 500.
    fn from(e: RandomError) -> Self {
        crate::error::ApiError::internal(e.to_string())
    }
}

/// A set of symbols [`Rng::string`] draws characters from.
///
/// Symbols are ASCII bytes. The named constants cover the cases services
/// keep re-deriving; [`Alphabet::new`] takes a custom one.
///
/// ```
/// use boogy_sdk::random::Alphabet;
/// assert_eq!(Alphabet::HEX.len(), 16);
/// assert_eq!(Alphabet::URL_SAFE.len(), 64);
/// let custom = Alphabet::new("abc").unwrap();
/// assert_eq!(custom.len(), 3);
/// ```
///
/// A duplicated symbol is allowed and simply weights that symbol; the
/// draw is uniform over *positions*, not over distinct characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alphabet<'a> {
    symbols: &'a [u8],
}

impl Alphabet<'static> {
    /// Lowercase hex, 16 symbols — `0123456789abcdef`. 4 bits per
    /// character. The conventional choice for short slugs and for
    /// rendering byte strings.
    pub const HEX: Alphabet<'static> = Alphabet {
        symbols: b"0123456789abcdef",
    };

    /// Uppercase hex, 16 symbols — `0123456789ABCDEF`.
    pub const HEX_UPPER: Alphabet<'static> = Alphabet {
        symbols: b"0123456789ABCDEF",
    };

    /// Decimal digits, 10 symbols. For numeric one-time codes; note that
    /// a 6-digit code is only ~20 bits and needs rate limiting, not
    /// entropy, to be safe.
    pub const DIGITS: Alphabet<'static> = Alphabet {
        symbols: b"0123456789",
    };

    /// Crockford base32, 32 symbols — digits plus uppercase letters with
    /// `I`, `L`, `O` and `U` removed. 5 bits per character, and the
    /// removals mean a human can read a code aloud without confusing
    /// `1`/`I` or `0`/`O`. Use it for anything a person retypes.
    pub const BASE32_CROCKFORD: Alphabet<'static> = Alphabet {
        symbols: b"0123456789ABCDEFGHJKMNPQRSTVWXYZ",
    };

    /// Digits and both letter cases, 62 symbols. ~5.95 bits per
    /// character. Compact but case-sensitive — a poor choice for
    /// anything typed by hand.
    pub const ALPHANUMERIC: Alphabet<'static> = Alphabet {
        symbols: b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz",
    };

    /// Digits, both letter cases, `-` and `_` — 64 symbols, exactly 6
    /// bits per character, and every symbol is safe in a URL path or
    /// query without escaping. The default behind [`Rng::id`].
    pub const URL_SAFE: Alphabet<'static> = Alphabet {
        symbols: b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_",
    };

    /// Lowercase letters and digits, 36 symbols. Case-insensitive, so it
    /// survives a round trip through a system that lowercases (email
    /// clients, some DNS tooling, careless copy-paste).
    pub const LOWERCASE_ALPHANUMERIC: Alphabet<'static> = Alphabet {
        symbols: b"0123456789abcdefghijklmnopqrstuvwxyz",
    };
}

impl<'a> Alphabet<'a> {
    /// Build an alphabet from a string of symbols.
    ///
    /// Returns [`RandomError::EmptyAlphabet`] for `""`,
    /// [`RandomError::NonAsciiAlphabet`] if any byte is non-ASCII, and
    /// [`RandomError::AlphabetTooLarge`] beyond 256 symbols.
    pub fn new(symbols: &'a str) -> Result<Self, RandomError> {
        Self::from_bytes(symbols.as_bytes())
    }

    /// Build an alphabet from raw bytes. Same validation as
    /// [`Alphabet::new`].
    pub fn from_bytes(symbols: &'a [u8]) -> Result<Self, RandomError> {
        if symbols.is_empty() {
            return Err(RandomError::EmptyAlphabet);
        }
        if symbols.len() > 256 {
            return Err(RandomError::AlphabetTooLarge);
        }
        if symbols.iter().any(|b| !b.is_ascii()) {
            return Err(RandomError::NonAsciiAlphabet);
        }
        Ok(Alphabet { symbols })
    }

    /// Number of symbols.
    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    /// Whether the alphabet has no symbols. Only reachable for an
    /// alphabet built by hand; [`Alphabet::new`] rejects empty input.
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// The symbols, in order.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.symbols
    }

    /// The symbols as a string. Always succeeds for an alphabet built
    /// through [`Alphabet::new`] or a constant (both are ASCII).
    pub fn as_str(&self) -> &'a str {
        std::str::from_utf8(self.symbols).unwrap_or("")
    }

    /// Validate an alphabet that may have been built by hand.
    fn validated(&self) -> Result<(), RandomError> {
        if self.symbols.is_empty() {
            return Err(RandomError::EmptyAlphabet);
        }
        if self.symbols.len() > 256 {
            return Err(RandomError::AlphabetTooLarge);
        }
        if self.symbols.iter().any(|b| !b.is_ascii()) {
            return Err(RandomError::NonAsciiAlphabet);
        }
        Ok(())
    }
}

/// Rejection sampling draws again when a candidate falls outside the
/// range. With a real random source each round succeeds with probability
/// above 1/2, so the chance of exhausting this many rounds is below
/// 2^-100 — unreachable in practice. The cap exists only so that a degenerate
/// source (a stuck bit, a source that returns one constant byte forever)
/// cannot spin the guest until its CPU budget trips. On exhaustion the
/// helpers fall back to a modulo reduction: still in range, no longer
/// perfectly uniform, but terminating.
const MAX_REJECTION_ROUNDS: usize = 100;

/// Default length of [`Rng::id`], in characters. 22 URL-safe characters
/// at 6 bits each is 132 bits of value space — the same order as a UUID,
/// and 10 characters shorter to render.
const DEFAULT_ID_LEN: usize = 22;

/// Random values drawn from a [`ByteSource`].
///
/// This is the whole algorithmic surface of the module; the free
/// functions the glue emits into a service crate are one-line wrappers
/// over these methods. Construct one per use — it is a thin wrapper over
/// the source and holds no state of its own.
///
/// ```
/// use boogy_sdk::random::{Alphabet, Rng};
///
/// // Any FnMut(usize) -> Vec<u8> is a source; a service uses the host's.
/// let mut rng = Rng::new(|n: usize| vec![0u8; n]);
/// assert_eq!(rng.int(1, 6), 1);
/// assert_eq!(rng.string(4, &Alphabet::HEX), "0000");
/// ```
pub struct Rng<S: ByteSource> {
    source: S,
}

impl<S: ByteSource> Rng<S> {
    /// Wrap a byte source.
    pub fn new(source: S) -> Self {
        Rng { source }
    }

    /// Unwrap back to the byte source.
    pub fn into_inner(self) -> S {
        self.source
    }

    // ─── Raw bytes ──────────────────────────────────────────────────────

    /// Exactly `n` random bytes.
    ///
    /// If the source returns fewer bytes than requested — which the host
    /// does when `n` exceeds its per-call cap — the result is zero-padded
    /// to `n` rather than short. That keeps every caller's length
    /// arithmetic total; it does not manufacture entropy, so do not ask
    /// for absurd lengths and assume they are all random.
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut out = self.source.next_bytes(n);
        if out.len() > n {
            out.truncate(n);
        } else if out.len() < n {
            out.resize(n, 0);
        }
        out
    }

    /// A uniformly random `u64` over the full 64-bit range.
    pub fn u64(&mut self) -> u64 {
        let b = self.bytes(8);
        let mut v = 0u64;
        for byte in b {
            v = (v << 8) | byte as u64;
        }
        v
    }

    /// A uniformly random `u64` in `[0, bound)` — **free of modulo
    /// bias**.
    ///
    /// This is the primitive every other range helper is built on. It
    /// draws only as many bytes as `bound` needs, masks them down to the
    /// bit width of `bound - 1`, and **rejects** any candidate that lands
    /// at or above `bound`, drawing again. That rejection is the whole
    /// point: the naive `bytes[0] % bound` maps more byte values onto
    /// small results than large ones whenever `bound` does not divide
    /// 256, which makes low results measurably more likely.
    ///
    /// Returns `0` for `bound` of `0` or `1` (the only value either can
    /// mean) without consuming any bytes.
    pub fn u64_below(&mut self, bound: u64) -> u64 {
        if bound <= 1 {
            return 0;
        }
        // Bit width of the largest value we may return, and the number of
        // whole bytes that covers. Masking to the exact bit width (rather
        // than to the byte boundary) is what keeps the rejection rate
        // under 50%: at most half the masked space can fall out of range.
        let bits = u64::BITS - (bound - 1).leading_zeros();
        let n_bytes = bits.div_ceil(8) as usize;
        let mask = if bits >= u64::BITS {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };

        let mut candidate = 0u64;
        for _ in 0..MAX_REJECTION_ROUNDS {
            let b = self.bytes(n_bytes);
            let mut v = 0u64;
            for byte in b {
                v = (v << 8) | byte as u64;
            }
            candidate = v & mask;
            if candidate < bound {
                return candidate;
            }
        }
        // Degenerate source only — see MAX_REJECTION_ROUNDS.
        candidate % bound
    }

    // ─── Integers ───────────────────────────────────────────────────────

    /// A uniformly random `i64` in `[min, max]` — **both ends
    /// inclusive**, no modulo bias.
    ///
    /// Returns [`RandomError::EmptyRange`] when `min > max`. `min == max`
    /// is a valid range of one value and consumes no entropy.
    pub fn try_int(&mut self, min: i64, max: i64) -> Result<i64, RandomError> {
        if min > max {
            return Err(RandomError::EmptyRange);
        }
        // Width in i128 so the full i64 range (span == 2^64) is
        // representable rather than wrapping to 0.
        let span = (max as i128) - (min as i128) + 1;
        let offset = if span > u64::MAX as i128 {
            // Only reachable for min == i64::MIN, max == i64::MAX.
            self.u64() as i128
        } else {
            self.u64_below(span as u64) as i128
        };
        Ok((min as i128 + offset) as i64)
    }

    /// A uniformly random `i64` in `[min, max]`, both ends inclusive.
    ///
    /// # Panics
    ///
    /// If `min > max`. A panic inside a service traps the guest and
    /// fails the request — use [`Rng::try_int`] when the bounds come
    /// from request input rather than from literals.
    pub fn int(&mut self, min: i64, max: i64) -> i64 {
        self.try_int(min, max)
            .expect("random int: min must not be greater than max")
    }

    /// A uniformly random `i64` in `[start, end)` — **end exclusive**,
    /// the half-open form that matches slice indexing.
    ///
    /// Returns [`RandomError::EmptyRange`] when `end <= start`.
    pub fn try_int_exclusive(&mut self, start: i64, end: i64) -> Result<i64, RandomError> {
        if end <= start {
            return Err(RandomError::EmptyRange);
        }
        self.try_int(start, end - 1)
    }

    /// A uniformly random `i64` in `[start, end)`, end exclusive.
    ///
    /// # Panics
    ///
    /// If `end <= start`. See [`Rng::try_int_exclusive`].
    pub fn int_exclusive(&mut self, start: i64, end: i64) -> i64 {
        self.try_int_exclusive(start, end)
            .expect("random int: end must be greater than start")
    }

    // ─── Floats ─────────────────────────────────────────────────────────

    /// A uniformly random `f64` in `[0.0, 1.0)`.
    ///
    /// Built from the top 53 bits of an 8-byte draw — every value is a
    /// multiple of 2^-53, which is the finest spacing an `f64` can
    /// represent uniformly across the unit interval.
    pub fn unit_float(&mut self) -> f64 {
        // 53 bits = f64 mantissa width; scaling by 2^-53 is exact.
        ((self.u64() >> 11) as f64) * (1.0f64 / (1u64 << 53) as f64)
    }

    /// A uniformly random `f64` in `[min, max)`.
    ///
    /// `max` is exclusive in the sense that the underlying draw is; at
    /// extreme magnitudes floating-point rounding can still land on
    /// `max`. `min == max` returns `min`.
    ///
    /// Returns [`RandomError::EmptyRange`] when `min > max` and
    /// [`RandomError::NonFiniteBound`] when either bound is NaN/infinite
    /// or the span between them overflows to infinity.
    pub fn try_float(&mut self, min: f64, max: f64) -> Result<f64, RandomError> {
        if !min.is_finite() || !max.is_finite() {
            return Err(RandomError::NonFiniteBound);
        }
        if min > max {
            return Err(RandomError::EmptyRange);
        }
        let span = max - min;
        if !span.is_finite() {
            return Err(RandomError::NonFiniteBound);
        }
        if span == 0.0 {
            return Ok(min);
        }
        Ok(min + span * self.unit_float())
    }

    /// A uniformly random `f64` in `[min, max)`.
    ///
    /// # Panics
    ///
    /// If `min > max` or either bound is not finite. See
    /// [`Rng::try_float`].
    pub fn float(&mut self, min: f64, max: f64) -> f64 {
        self.try_float(min, max)
            .expect("random float: bounds must be finite with min <= max")
    }

    // ─── Booleans ───────────────────────────────────────────────────────

    /// `true` or `false`, each with probability 1/2.
    pub fn bool(&mut self) -> bool {
        self.bytes(1)[0] & 1 == 1
    }

    /// `true` with probability `p`.
    ///
    /// `p` is clamped: `p <= 0.0` is always `false`, `p >= 1.0` is always
    /// `true`, and NaN is `false`. Both saturating cases consume no
    /// entropy, which also makes a feature flag at 0% free.
    pub fn bool_with_probability(&mut self, p: f64) -> bool {
        if p.is_nan() || p <= 0.0 {
            return false;
        }
        if p >= 1.0 {
            return true;
        }
        self.unit_float() < p
    }

    // ─── Strings ────────────────────────────────────────────────────────

    /// A random string of exactly `len` characters drawn uniformly from
    /// `alphabet`.
    ///
    /// Unbiased: each character is drawn by masking a byte to the
    /// alphabet's bit width and rejecting out-of-range values, so a
    /// 62-symbol alphabet is as uniform as a 64-symbol one. Bytes are
    /// requested in batches rather than one per character, so a long
    /// string is a small number of entropy calls, not `len` of them.
    ///
    /// Returns the alphabet-validation errors from [`Alphabet::new`].
    /// `len == 0` returns `""`.
    pub fn try_string(&mut self, len: usize, alphabet: &Alphabet<'_>) -> Result<String, RandomError> {
        alphabet.validated()?;
        if len == 0 {
            return Ok(String::new());
        }
        let symbols = alphabet.as_bytes();
        let n = symbols.len() as u64;
        // Same masked-rejection scheme as `u64_below`, applied per byte.
        // A one-symbol alphabet lands here with a zero mask, which always
        // selects index 0 — no special case needed.
        let bits = u64::BITS - (n - 1).leading_zeros();
        let mask = ((1u64 << bits) - 1) as u8;

        let mut out = Vec::with_capacity(len);
        let mut rounds = 0usize;
        while out.len() < len {
            if rounds >= MAX_REJECTION_ROUNDS {
                // Degenerate source only — see MAX_REJECTION_ROUNDS.
                let remaining = len - out.len();
                for b in self.bytes(remaining) {
                    out.push(symbols[(b as u64 % n) as usize]);
                }
                break;
            }
            rounds += 1;
            // Over-request so one round usually finishes the string: the
            // acceptance rate is above 1/2 by construction of `mask`.
            let need = len - out.len();
            for b in self.bytes(need * 2) {
                let idx = (b & mask) as u64;
                if idx < n {
                    out.push(symbols[idx as usize]);
                    if out.len() == len {
                        break;
                    }
                }
            }
        }
        // Every symbol was validated ASCII, so this cannot fail.
        Ok(String::from_utf8(out).unwrap_or_default())
    }

    /// A random string of exactly `len` characters from `alphabet`.
    ///
    /// # Panics
    ///
    /// If the alphabet is empty, non-ASCII, or over 256 symbols. The
    /// constants on [`Alphabet`] never panic; see [`Rng::try_string`] for
    /// a custom alphabet built from input.
    pub fn string(&mut self, len: usize, alphabet: &Alphabet<'_>) -> String {
        self.try_string(len, alphabet)
            .expect("random string: alphabet must be non-empty ASCII of at most 256 symbols")
    }

    /// An opaque public id: 22 URL-safe characters, ~131 bits.
    ///
    /// The default answer for "I need a user-facing id that does not leak
    /// how many rows I have". Safe anywhere in a URL, shorter than a
    /// UUID, and wide enough that collisions are not a design concern —
    /// though the column should still carry a unique index.
    pub fn id(&mut self) -> String {
        self.string(DEFAULT_ID_LEN, &Alphabet::URL_SAFE)
    }

    /// A lowercase hex string of exactly `len` characters (`len / 2`
    /// bytes of entropy).
    pub fn hex(&mut self, len: usize) -> String {
        self.string(len, &Alphabet::HEX)
    }

    // ─── Collections ────────────────────────────────────────────────────

    /// `n` values, each produced by calling `f` with this generator.
    ///
    /// ```
    /// use boogy_sdk::random::Rng;
    /// let mut rng = Rng::new(|n: usize| vec![0u8; n]);
    /// let rolls = rng.vec_of(3, |r| r.int(1, 6));
    /// assert_eq!(rolls, vec![1, 1, 1]);
    /// ```
    pub fn vec_of<T, F>(&mut self, n: usize, mut f: F) -> Vec<T>
    where
        F: FnMut(&mut Self) -> T,
    {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(f(self));
        }
        out
    }

    /// A uniformly random index in `[0, len)`, or `None` when `len == 0`.
    pub fn choose_index(&mut self, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        Some(self.u64_below(len as u64) as usize)
    }

    /// One element of `items`, chosen uniformly. `None` for an empty
    /// slice — an empty slice is a normal state, not an error.
    pub fn choose<'t, T>(&mut self, items: &'t [T]) -> Option<&'t T> {
        self.choose_index(items.len()).map(|i| &items[i])
    }

    /// Shuffle `items` in place into a uniformly random permutation.
    ///
    /// Fisher-Yates, drawing each swap partner with [`Rng::u64_below`],
    /// so every one of the `len!` orderings is equally likely. Slices of
    /// 0 or 1 elements are left untouched and consume no entropy.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        let len = items.len();
        if len < 2 {
            return;
        }
        for i in (1..len).rev() {
            let j = self.u64_below((i + 1) as u64) as usize;
            items.swap(i, j);
        }
    }

    /// `k` distinct indices in `[0, len)`, in random order.
    ///
    /// When `k >= len` this returns all `len` indices, shuffled — a
    /// documented saturate rather than an error, so callers do not have
    /// to bound `k` against the collection first.
    pub fn sample_indices(&mut self, len: usize, k: usize) -> Vec<usize> {
        let take = k.min(len);
        let mut pool: Vec<usize> = (0..len).collect();
        // Partial Fisher-Yates: after `take` steps the prefix is a
        // uniform sample without replacement.
        for i in 0..take {
            let j = i + self.u64_below((len - i) as u64) as usize;
            pool.swap(i, j);
        }
        pool.truncate(take);
        pool
    }

    /// `k` distinct elements of `items`, in random order — sampling
    /// **without replacement**, so no element appears twice.
    ///
    /// Returns everything (shuffled) when `k >= items.len()`, and an
    /// empty vec for an empty slice. Use [`Rng::vec_of`] with
    /// [`Rng::choose`] if you want sampling *with* replacement.
    pub fn sample<'t, T>(&mut self, items: &'t [T], k: usize) -> Vec<&'t T> {
        self.sample_indices(items.len(), k)
            .into_iter()
            .map(|i| &items[i])
            .collect()
    }

    // ─── UUIDs ──────────────────────────────────────────────────────────

    /// A random (version 4) UUID as 16 raw bytes, with the version and
    /// variant bits set.
    pub fn uuid_v4_bytes(&mut self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b.copy_from_slice(&self.bytes(16));
        b[6] = (b[6] & 0x0f) | 0x40; // version 4
        b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
        b
    }

    /// A random (version 4) UUID in the canonical hyphenated form,
    /// lowercase — `f47ac10b-58cc-4372-a567-0e02b2c3d479`.
    ///
    /// 122 bits of entropy. The right choice for an id that must be
    /// recognisable as a UUID to something outside the service.
    pub fn uuid_v4(&mut self) -> String {
        format_uuid(&self.uuid_v4_bytes())
    }

    /// A time-ordered (version 7) UUID as 16 raw bytes: a 48-bit
    /// big-endian millisecond timestamp followed by random bits.
    ///
    /// `unix_millis` is **caller-supplied** — pass `now_millis()`. This
    /// module never reads the clock itself, so a service that wants v7
    /// ids grants `clock` explicitly and a service that does not is
    /// unaffected. Only the low 48 bits of `unix_millis` are used.
    pub fn uuid_v7_bytes(&mut self, unix_millis: u64) -> [u8; 16] {
        let mut b = [0u8; 16];
        let ts = unix_millis & 0x0000_ffff_ffff_ffff;
        b[0..6].copy_from_slice(&ts.to_be_bytes()[2..8]);
        b[6..16].copy_from_slice(&self.bytes(10));
        b[6] = (b[6] & 0x0f) | 0x70; // version 7
        b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
        b
    }

    /// A time-ordered (version 7) UUID in canonical hyphenated form.
    ///
    /// Sorts lexicographically by creation time, which keeps inserts
    /// clustered instead of scattered — prefer it over v4 for a primary
    /// or index key. See [`Rng::uuid_v7_bytes`] on the caller-supplied
    /// timestamp.
    pub fn uuid_v7(&mut self, unix_millis: u64) -> String {
        format_uuid(&self.uuid_v7_bytes(unix_millis))
    }
}

/// Render 16 bytes as a canonical lowercase hyphenated UUID string.
///
/// Pure formatting — it sets no version or variant bits, so the caller
/// decides what the bytes mean.
pub fn format_uuid(bytes: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::rc::Rc;

    // ─── Deterministic / adversarial byte sources ───────────────────────

    /// Emits 0, 1, 2, … 255, 0, 1, … and counts bytes served. The
    /// counter is what makes the bias test below possible: it lets a
    /// test assert how much of the byte stream a draw consumed, which is
    /// exactly where rejection sampling and modulo differ.
    #[derive(Clone, Default)]
    struct Sequential {
        served: Rc<RefCell<usize>>,
    }

    impl Sequential {
        fn served(&self) -> usize {
            *self.served.borrow()
        }
    }

    impl ByteSource for Sequential {
        fn next_bytes(&mut self, n: usize) -> Vec<u8> {
            let mut served = self.served.borrow_mut();
            let out = (0..n).map(|i| ((*served + i) % 256) as u8).collect();
            *served += n;
            out
        }
    }

    /// Always the same byte. `Constant(0xff)` is the adversarial case for
    /// rejection sampling: with a bound that does not fill its bit width,
    /// every candidate is rejected forever unless the round cap catches it.
    struct Constant(u8);

    impl ByteSource for Constant {
        fn next_bytes(&mut self, n: usize) -> Vec<u8> {
            vec![self.0; n]
        }
    }

    /// Returns fewer bytes than asked for (half, rounded down), so the
    /// zero-padding path is exercised on every call.
    struct Short;

    impl ByteSource for Short {
        fn next_bytes(&mut self, n: usize) -> Vec<u8> {
            vec![0xab; n / 2]
        }
    }

    /// Returns nothing at all — the shape of a denied `entropy`
    /// capability taken to its limit.
    struct Empty;

    impl ByteSource for Empty {
        fn next_bytes(&mut self, _n: usize) -> Vec<u8> {
            Vec::new()
        }
    }

    /// A deterministic pseudo-random stream (xorshift64*). Not
    /// cryptographic and not the platform source — a reproducible stand-in
    /// so distribution tests are stable across runs.
    struct Xorshift(u64);

    impl ByteSource for Xorshift {
        fn next_bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n)
                .map(|_| {
                    let mut x = self.0;
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    self.0 = x;
                    (x >> 24) as u8
                })
                .collect()
        }
    }

    fn seeded() -> Rng<Xorshift> {
        Rng::new(Xorshift(0x2545_F491_4F6C_DD1D))
    }

    // ─── The property that matters: no modulo bias ──────────────────────

    // The bias test. A biased implementation (`byte % bound`) passes any
    // "is it in range" check, and — for a stream of consecutive bytes —
    // even passes a naive count check, because `b % 200` over any whole
    // multiple of 200 consecutive bytes is perfectly balanced. So counts
    // alone cannot separate the two. Two things can, and this asserts
    // both:
    //
    //   1. The mapping itself. Bound 200 needs 8 bits, so one byte is
    //      drawn and used as-is when it is in range. For a byte at or
    //      above the bound the correct behaviour is to DISCARD it and
    //      draw again; `b % 200` instead folds 200..=255 back onto
    //      0..=55, which is exactly the bias. Feeding a one-byte script
    //      followed by zeroes makes the two answers differ for 56 of the
    //      256 possible inputs.
    //
    //   2. Byte consumption. Rejection spends bytes that produce no
    //      value; modulo never does. Over 400 draws from a stream that
    //      cycles 0..=255, rejection consumes 456 bytes (400 accepted +
    //      56 discarded) where modulo would consume exactly 400.
    #[test]
    fn u64_below_is_free_of_modulo_bias_over_a_non_power_of_two_bound() {
        const BOUND: u64 = 200;

        // (1) Exhaustive single-byte mapping.
        for b in 0u16..=255 {
            let first = b as u8;
            let mut used = false;
            let mut rng = Rng::new(|n: usize| {
                let out = (0..n)
                    .map(|i| if !used && i == 0 { first } else { 0 })
                    .collect();
                used = true;
                out
            });
            let v = rng.u64_below(BOUND);
            if (b as u64) < BOUND {
                assert_eq!(v as u16, b, "in-range byte {b} must be used as-is");
            } else {
                assert_eq!(
                    v, 0,
                    "out-of-range byte {b} must be discarded and redrawn \
                     (a modulo mapping would fold it onto {})",
                    b as u64 % BOUND
                );
            }
        }

        // (2) Uniformity + consumption over a stream of consecutive bytes.
        let src = Sequential::default();
        let probe = src.clone();
        let mut rng = Rng::new(src);
        let mut counts = [0u32; BOUND as usize];
        for _ in 0..400 {
            let v = rng.u64_below(BOUND);
            assert!(v < BOUND, "drew {v}, out of range");
            counts[v as usize] += 1;
        }
        for (v, c) in counts.iter().enumerate() {
            assert_eq!(*c, 2, "value {v} appeared {c} times, expected 2");
        }
        assert_eq!(
            probe.served(),
            456,
            "400 draws must discard the 56 out-of-range bytes in the cycle \
             (a modulo mapping would consume exactly 400)"
        );
    }

    // The same property one byte wider: bound 1000 needs 10 bits, so
    // each draw takes 2 bytes masked to 0x3FF. Enumerating all 65536
    // two-byte inputs, every value in 0..1000 must be reachable from
    // exactly 64 of them (65536 / 1024 masked values, times 64 per
    // value). Any modulo reduction skews this.
    #[test]
    fn u64_below_maps_multi_byte_draws_uniformly() {
        const BOUND: u64 = 1000;
        let mut counts = HashMap::new();
        for hi in 0u16..=255 {
            for lo in 0u16..=255 {
                let script = [hi as u8, lo as u8];
                let mut idx = 0usize;
                // A source that replays the two bytes, then zeroes (a
                // zero candidate is always accepted, so a rejected pair
                // resolves on the next round without perturbing counts
                // we assert on).
                let mut rng = Rng::new(|n: usize| {
                    (0..n)
                        .map(|_| {
                            let b = script.get(idx).copied().unwrap_or(0);
                            idx += 1;
                            b
                        })
                        .collect()
                });
                let raw = (((hi as u64) << 8) | lo as u64) & 0x3ff;
                if raw < BOUND {
                    let v = rng.u64_below(BOUND);
                    // The accepted mapping is the identity on the masked
                    // draw — no reduction, no folding of high values onto
                    // low ones.
                    assert_eq!(v, raw, "input {hi:02x}{lo:02x} mapped to {v}");
                    *counts.entry(v).or_insert(0u32) += 1;
                }
            }
        }
        assert_eq!(counts.len(), BOUND as usize, "not every value is reachable");
        for (v, c) in &counts {
            assert_eq!(*c, 64, "value {v} is reachable from {c} inputs, expected 64");
        }
    }

    #[test]
    fn int_over_a_non_power_of_two_range_is_uniform_in_aggregate() {
        // Distribution smoke test over a pseudo-random stream: 3 values,
        // 30_000 draws, each expected ~10_000.
        let mut rng = seeded();
        let mut counts = [0u32; 3];
        for _ in 0..30_000 {
            counts[rng.int(0, 2) as usize] += 1;
        }
        for (v, c) in counts.iter().enumerate() {
            assert!(
                (9_000..=11_000).contains(c),
                "value {v} drawn {c} times, expected ~10000"
            );
        }
    }

    // ─── Ranges: totality and edges ─────────────────────────────────────

    #[test]
    fn int_range_is_inclusive_of_both_ends() {
        let mut rng = seeded();
        let mut saw_min = false;
        let mut saw_max = false;
        for _ in 0..500 {
            let v = rng.int(1, 6);
            assert!((1..=6).contains(&v), "drew {v}");
            saw_min |= v == 1;
            saw_max |= v == 6;
        }
        assert!(saw_min && saw_max, "range ends must both be reachable");
    }

    #[test]
    fn single_value_range_returns_that_value_without_consuming_entropy() {
        let src = Sequential::default();
        let probe = src.clone();
        let mut rng = Rng::new(src);
        assert_eq!(rng.int(7, 7), 7);
        assert_eq!(rng.try_int(-3, -3), Ok(-3));
        assert_eq!(probe.served(), 0);
    }

    #[test]
    fn reversed_range_is_an_error_not_a_wraparound() {
        let mut rng = seeded();
        assert_eq!(rng.try_int(6, 5), Err(RandomError::EmptyRange));
        assert_eq!(rng.try_int(i64::MAX, i64::MIN), Err(RandomError::EmptyRange));
        assert_eq!(rng.try_int_exclusive(5, 5), Err(RandomError::EmptyRange));
        assert_eq!(rng.try_int_exclusive(5, 4), Err(RandomError::EmptyRange));
    }

    #[test]
    #[should_panic(expected = "min must not be greater than max")]
    fn int_panics_on_a_reversed_range() {
        seeded().int(6, 5);
    }

    #[test]
    #[should_panic(expected = "end must be greater than start")]
    fn int_exclusive_panics_on_an_empty_range() {
        seeded().int_exclusive(5, 5);
    }

    #[test]
    fn int_exclusive_never_returns_the_end() {
        let mut rng = seeded();
        let mut saw_start = false;
        for _ in 0..500 {
            let v = rng.int_exclusive(0, 3);
            assert!((0..3).contains(&v), "drew {v}");
            saw_start |= v == 0;
        }
        assert!(saw_start);
    }

    #[test]
    fn int_spans_the_full_i64_range_without_overflow() {
        let mut rng = seeded();
        for _ in 0..1_000 {
            // Only asserting it terminates and stays representable; the
            // span here is 2^64 and must not wrap to an empty bound.
            let _ = rng.int(i64::MIN, i64::MAX);
        }
        assert_eq!(rng.try_int(i64::MIN, i64::MIN), Ok(i64::MIN));
        assert_eq!(rng.try_int(i64::MAX, i64::MAX), Ok(i64::MAX));
    }

    #[test]
    fn negative_ranges_work() {
        let mut rng = seeded();
        for _ in 0..200 {
            let v = rng.int(-10, -5);
            assert!((-10..=-5).contains(&v), "drew {v}");
        }
    }

    // ─── Floats ─────────────────────────────────────────────────────────

    #[test]
    fn unit_float_stays_in_the_half_open_unit_interval() {
        let mut rng = seeded();
        for _ in 0..2_000 {
            let v = rng.unit_float();
            assert!((0.0..1.0).contains(&v), "drew {v}");
        }
        // All-ones bytes are the largest possible draw and must still be
        // below 1.0.
        assert!(Rng::new(Constant(0xff)).unit_float() < 1.0);
        assert_eq!(Rng::new(Constant(0x00)).unit_float(), 0.0);
    }

    #[test]
    fn float_stays_within_its_bounds() {
        let mut rng = seeded();
        for _ in 0..2_000 {
            let v = rng.float(-2.5, 7.5);
            assert!((-2.5..=7.5).contains(&v), "drew {v}");
        }
    }

    #[test]
    fn float_edge_cases_are_total() {
        let mut rng = seeded();
        assert_eq!(rng.try_float(1.5, 1.5), Ok(1.5));
        assert_eq!(rng.try_float(2.0, 1.0), Err(RandomError::EmptyRange));
        assert_eq!(
            rng.try_float(f64::NAN, 1.0),
            Err(RandomError::NonFiniteBound)
        );
        assert_eq!(
            rng.try_float(0.0, f64::INFINITY),
            Err(RandomError::NonFiniteBound)
        );
        // Finite bounds whose span overflows.
        assert_eq!(
            rng.try_float(f64::MIN, f64::MAX),
            Err(RandomError::NonFiniteBound)
        );
    }

    // ─── Booleans ───────────────────────────────────────────────────────

    #[test]
    fn bool_is_balanced() {
        let mut rng = seeded();
        let trues = (0..10_000).filter(|_| rng.bool()).count();
        assert!((4_600..=5_400).contains(&trues), "{trues} trues in 10000");
    }

    #[test]
    fn bool_with_probability_saturates_without_entropy() {
        let src = Sequential::default();
        let probe = src.clone();
        let mut rng = Rng::new(src);
        assert!(!rng.bool_with_probability(0.0));
        assert!(!rng.bool_with_probability(-1.0));
        assert!(!rng.bool_with_probability(f64::NAN));
        assert!(rng.bool_with_probability(1.0));
        assert!(rng.bool_with_probability(2.0));
        assert_eq!(probe.served(), 0);
    }

    #[test]
    fn bool_with_probability_tracks_p() {
        let mut rng = seeded();
        let trues = (0..10_000)
            .filter(|_| rng.bool_with_probability(0.25))
            .count();
        assert!((2_200..=2_800).contains(&trues), "{trues} trues in 10000");
    }

    // ─── Strings and alphabets ──────────────────────────────────────────

    #[test]
    fn string_has_the_requested_length_and_only_alphabet_symbols() {
        let mut rng = seeded();
        for alphabet in [
            Alphabet::HEX,
            Alphabet::HEX_UPPER,
            Alphabet::DIGITS,
            Alphabet::BASE32_CROCKFORD,
            Alphabet::ALPHANUMERIC,
            Alphabet::URL_SAFE,
            Alphabet::LOWERCASE_ALPHANUMERIC,
        ] {
            for len in [0, 1, 6, 22, 64] {
                let s = rng.string(len, &alphabet);
                assert_eq!(s.len(), len, "wrong length for {}", alphabet.as_str());
                assert!(
                    s.bytes().all(|b| alphabet.as_bytes().contains(&b)),
                    "{s} contains a symbol outside {}",
                    alphabet.as_str()
                );
            }
        }
    }

    #[test]
    fn string_draws_each_symbol_uniformly_from_a_non_power_of_two_alphabet() {
        // 62 symbols is the case a naive `byte % 62` biases hardest.
        let mut rng = seeded();
        let s = rng.string(62_000, &Alphabet::ALPHANUMERIC);
        let mut counts = HashMap::new();
        for b in s.bytes() {
            *counts.entry(b).or_insert(0u32) += 1;
        }
        assert_eq!(counts.len(), 62, "not every symbol appeared");
        for (b, c) in &counts {
            assert!(
                (850..=1_150).contains(c),
                "symbol {} appeared {c} times, expected ~1000",
                *b as char
            );
        }
    }

    #[test]
    fn single_symbol_alphabet_repeats_that_symbol() {
        let mut rng = seeded();
        let a = Alphabet::new("x").unwrap();
        assert_eq!(rng.string(5, &a), "xxxxx");
    }

    #[test]
    fn alphabet_validation_rejects_unusable_input() {
        assert_eq!(Alphabet::new(""), Err(RandomError::EmptyAlphabet));
        assert_eq!(Alphabet::new("abcé"), Err(RandomError::NonAsciiAlphabet));
        let too_many: String = "a".repeat(257);
        assert_eq!(
            Alphabet::new(&too_many),
            Err(RandomError::AlphabetTooLarge)
        );
        assert!(Alphabet::new("ab").is_ok());
    }

    #[test]
    fn try_string_surfaces_alphabet_errors_instead_of_panicking() {
        let mut rng = seeded();
        let empty = Alphabet::from_bytes(b"abc").unwrap();
        assert!(rng.try_string(4, &empty).is_ok());
        // Reconstruct an invalid alphabet the way a hand-built one could be.
        let bad = Alphabet { symbols: &[] };
        assert_eq!(rng.try_string(4, &bad), Err(RandomError::EmptyAlphabet));
    }

    #[test]
    #[should_panic(expected = "alphabet must be non-empty")]
    fn string_panics_on_an_empty_alphabet() {
        seeded().string(4, &Alphabet { symbols: &[] });
    }

    #[test]
    fn id_and_hex_have_their_documented_shapes() {
        let mut rng = seeded();
        let id = rng.id();
        assert_eq!(id.len(), 22);
        assert!(id
            .bytes()
            .all(|b| Alphabet::URL_SAFE.as_bytes().contains(&b)));
        let h = rng.hex(12);
        assert_eq!(h.len(), 12);
        assert!(h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    #[test]
    fn ids_do_not_repeat() {
        let mut rng = seeded();
        let ids: HashSet<String> = (0..1_000).map(|_| rng.id()).collect();
        assert_eq!(ids.len(), 1_000);
    }

    // ─── Collections ────────────────────────────────────────────────────

    #[test]
    fn choose_returns_none_for_an_empty_slice() {
        let mut rng = seeded();
        let empty: [u8; 0] = [];
        assert!(rng.choose(&empty).is_none());
        assert!(rng.choose_index(0).is_none());
    }

    #[test]
    fn choose_covers_every_element() {
        let mut rng = seeded();
        let items = ["a", "b", "c"];
        let mut seen = HashSet::new();
        for _ in 0..300 {
            seen.insert(*rng.choose(&items).unwrap());
        }
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn choose_from_a_single_element_slice_is_that_element() {
        let mut rng = Rng::new(Empty);
        assert_eq!(rng.choose(&[42]), Some(&42));
    }

    #[test]
    fn shuffle_preserves_the_multiset() {
        let mut rng = seeded();
        for len in [0usize, 1, 2, 5, 50] {
            let mut v: Vec<usize> = (0..len).collect();
            rng.shuffle(&mut v);
            let mut sorted = v.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, (0..len).collect::<Vec<_>>());
        }
    }

    #[test]
    fn shuffle_reaches_every_permutation_roughly_equally() {
        // 4 elements = 24 permutations; 24_000 shuffles => ~1000 each.
        let mut rng = seeded();
        let mut counts: HashMap<Vec<u8>, u32> = HashMap::new();
        for _ in 0..24_000 {
            let mut v = vec![0u8, 1, 2, 3];
            rng.shuffle(&mut v);
            *counts.entry(v).or_insert(0) += 1;
        }
        assert_eq!(counts.len(), 24, "not every permutation occurred");
        for (perm, c) in &counts {
            assert!(
                (700..=1_300).contains(c),
                "permutation {perm:?} occurred {c} times, expected ~1000"
            );
        }
    }

    #[test]
    fn sample_returns_distinct_elements() {
        let mut rng = seeded();
        let items: Vec<u32> = (0..20).collect();
        for _ in 0..200 {
            let picked = rng.sample(&items, 5);
            assert_eq!(picked.len(), 5);
            let unique: HashSet<u32> = picked.iter().map(|v| **v).collect();
            assert_eq!(unique.len(), 5, "sample repeated an element");
        }
    }

    #[test]
    fn sample_saturates_when_k_exceeds_the_slice() {
        let mut rng = seeded();
        let items = [1, 2, 3];
        let picked = rng.sample(&items, 99);
        assert_eq!(picked.len(), 3);
        let unique: HashSet<i32> = picked.iter().map(|v| **v).collect();
        assert_eq!(unique.len(), 3);
        let empty: [i32; 0] = [];
        assert!(rng.sample(&empty, 4).is_empty());
        assert!(rng.sample(&items, 0).is_empty());
    }

    #[test]
    fn sample_is_unbiased_over_which_elements_are_chosen() {
        let mut rng = seeded();
        let items: Vec<u32> = (0..5).collect();
        let mut counts = [0u32; 5];
        for _ in 0..20_000 {
            for v in rng.sample(&items, 2) {
                counts[*v as usize] += 1;
            }
        }
        // Each element should appear in 2/5 of draws => ~8000.
        for (v, c) in counts.iter().enumerate() {
            assert!(
                (7_400..=8_600).contains(c),
                "element {v} sampled {c} times, expected ~8000"
            );
        }
    }

    #[test]
    fn vec_of_produces_the_requested_count() {
        let mut rng = seeded();
        assert_eq!(rng.vec_of(0, |r| r.int(1, 6)).len(), 0);
        let rolls = rng.vec_of(100, |r| r.int(1, 6));
        assert_eq!(rolls.len(), 100);
        assert!(rolls.iter().all(|v| (1..=6).contains(v)));
    }

    // ─── UUIDs ──────────────────────────────────────────────────────────

    #[test]
    fn uuid_v4_has_the_canonical_shape_and_bits() {
        let mut rng = seeded();
        for _ in 0..100 {
            let u = rng.uuid_v4();
            assert_eq!(u.len(), 36);
            let parts: Vec<&str> = u.split('-').collect();
            assert_eq!(
                parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
                vec![8, 4, 4, 4, 12]
            );
            assert!(u
                .bytes()
                .all(|b| b == b'-' || (b.is_ascii_hexdigit() && !b.is_ascii_uppercase())));
            assert_eq!(&parts[2][0..1], "4", "version nibble");
            assert!(
                matches!(&parts[3][0..1], "8" | "9" | "a" | "b"),
                "variant nibble in {u}"
            );
        }
    }

    #[test]
    fn uuid_v4_sets_version_bits_even_from_a_degenerate_source() {
        let b = Rng::new(Constant(0xff)).uuid_v4_bytes();
        assert_eq!(b[6] >> 4, 4);
        assert_eq!(b[8] >> 6, 0b10);
        let b = Rng::new(Empty).uuid_v4_bytes();
        assert_eq!(b[6] >> 4, 4);
        assert_eq!(b[8] >> 6, 0b10);
    }

    #[test]
    fn uuid_v4_values_do_not_repeat() {
        let mut rng = seeded();
        let seen: HashSet<String> = (0..1_000).map(|_| rng.uuid_v4()).collect();
        assert_eq!(seen.len(), 1_000);
    }

    #[test]
    fn uuid_v7_encodes_the_supplied_timestamp_and_sorts_by_it() {
        let mut rng = seeded();
        let ms: u64 = 1_767_225_600_000; // an arbitrary fixed instant
        let u = rng.uuid_v7(ms);
        let bytes = rng.uuid_v7_bytes(ms);
        let mut ts_bytes = [0u8; 8];
        ts_bytes[2..8].copy_from_slice(&bytes[0..6]);
        assert_eq!(u64::from_be_bytes(ts_bytes), ms);

        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(&parts[2][0..1], "7", "version nibble");
        assert!(matches!(&parts[3][0..1], "8" | "9" | "a" | "b"));

        // Later timestamps must sort after earlier ones, as strings.
        let a = rng.uuid_v7(ms);
        let b = rng.uuid_v7(ms + 1);
        let c = rng.uuid_v7(ms + 100_000);
        assert!(a < b && b < c, "v7 ids must be time-ordered: {a} {b} {c}");
    }

    #[test]
    fn uuid_v7_truncates_a_timestamp_beyond_48_bits() {
        let mut rng = seeded();
        // Only the low 48 bits are encoded; the call must not panic or
        // corrupt neighbouring bytes.
        let b = rng.uuid_v7_bytes(u64::MAX);
        assert_eq!(&b[0..6], &[0xff; 6]);
        assert_eq!(b[6] >> 4, 7);
    }

    #[test]
    fn format_uuid_is_stable() {
        let bytes = [
            0xf4, 0x7a, 0xc1, 0x0b, 0x58, 0xcc, 0x43, 0x72, 0xa5, 0x67, 0x0e, 0x02, 0xb2, 0xc3,
            0xd4, 0x79,
        ];
        assert_eq!(format_uuid(&bytes), "f47ac10b-58cc-4372-a567-0e02b2c3d479");
    }

    // ─── Degenerate sources terminate and stay in range ─────────────────

    #[test]
    fn a_short_read_is_zero_padded_to_the_requested_length() {
        let mut rng = Rng::new(Short);
        assert_eq!(rng.bytes(4), vec![0xab, 0xab, 0x00, 0x00]);
        assert_eq!(rng.bytes(1), vec![0x00]);
        assert_eq!(rng.bytes(0), Vec::<u8>::new());
    }

    #[test]
    fn an_empty_source_still_produces_in_range_values() {
        let mut rng = Rng::new(Empty);
        assert_eq!(rng.u64_below(200), 0);
        assert_eq!(rng.int(1, 6), 1);
        assert_eq!(rng.unit_float(), 0.0);
        assert!(!rng.bool());
        assert_eq!(rng.string(4, &Alphabet::HEX), "0000");
        assert_eq!(rng.id().len(), 22);
        let mut v = vec![1, 2, 3];
        rng.shuffle(&mut v);
        assert_eq!(v.len(), 3);
    }

    #[test]
    fn a_source_that_always_rejects_terminates_in_range() {
        // 0xff masked to 8 bits is 255, which is >= 200 forever. Without
        // the round cap this spins until the request budget trips.
        let mut rng = Rng::new(Constant(0xff));
        let v = rng.u64_below(200);
        assert!(v < 200, "fallback produced {v}");
        let v = rng.int(1, 6);
        assert!((1..=6).contains(&v), "fallback produced {v}");
        // Same for the string path (62 symbols, mask 0x3f -> 63 rejected).
        let s = rng.string(8, &Alphabet::ALPHANUMERIC);
        assert_eq!(s.len(), 8);
        assert!(s
            .bytes()
            .all(|b| Alphabet::ALPHANUMERIC.as_bytes().contains(&b)));
    }

    #[test]
    fn power_of_two_bounds_never_reject() {
        let src = Sequential::default();
        let probe = src.clone();
        let mut rng = Rng::new(src);
        for _ in 0..256 {
            assert!(rng.u64_below(256) < 256);
        }
        assert_eq!(probe.served(), 256, "a full-width bound must not reject");
    }

    #[test]
    fn u64_below_handles_degenerate_bounds() {
        let src = Sequential::default();
        let probe = src.clone();
        let mut rng = Rng::new(src);
        assert_eq!(rng.u64_below(0), 0);
        assert_eq!(rng.u64_below(1), 0);
        assert_eq!(probe.served(), 0);
        assert!(rng.u64_below(u64::MAX) < u64::MAX);
    }

    // ─── Error plumbing ─────────────────────────────────────────────────

    #[test]
    fn random_error_renders_and_maps_to_a_500() {
        assert_eq!(
            RandomError::EmptyRange.to_string(),
            "empty range: min is greater than max"
        );
        let api: crate::error::ApiError = RandomError::EmptyAlphabet.into();
        assert_eq!(api.status, 500);
    }

    #[test]
    fn a_closure_and_a_fn_pointer_are_both_byte_sources() {
        let mut from_closure = Rng::new(|n: usize| vec![1u8; n]);
        assert_eq!(from_closure.bytes(2), vec![1, 1]);

        fn source(n: usize) -> Vec<u8> {
            vec![2u8; n]
        }
        let mut from_fn = Rng::new(source as fn(usize) -> Vec<u8>);
        assert_eq!(from_fn.bytes(2), vec![2, 2]);
    }
}
