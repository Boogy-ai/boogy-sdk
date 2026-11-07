//! Reading an outbound request's refusals.
//!
//! The platform refuses an outbound request before sending it when a header
//! the guest set is one it reserves (`Cookie`, `Host`, any `X-Boogy-*`), or one
//! it fills itself on that request (a `secret-headers` name, or
//! `Authorization` when `connection-auth` is set). A guest-made
//! `Authorization` (for instance an ES256 JWT signed with `signing`) is sent as
//! written when the platform sets none.
//!
//! The refusal arrives as a `request-too-large` fetch error carrying
//! `"header-refused: <header>: <reason>"`. [`header_refused`] turns that
//! string into a value, so a guest matches a type rather than a prefix:
//!
//! ```ignore, ignore_snippet: the guest's generated outbound_http bindings are not in scope in the SDK's own docs.
//! match outbound_http::fetch(&request) {
//!     Err(outbound_http::FetchError::RequestTooLarge(msg)) => {
//!         if let Some(r) = boogy_sdk::outbound::header_refused(&msg) {
//!             // r.header is lowercased, r.reason says what to do instead.
//!         }
//!     }
//!     _ => {}
//! }
//! ```

/// A header the platform refused on an outbound request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderRefused {
    /// The refused header, lowercased.
    pub header: String,
    /// Why, and what to do instead.
    pub reason: String,
}

/// The prefix the refusal carries: `header-refused: <header>: <reason>`.
pub const HEADER_REFUSED_PREFIX: &str = "header-refused: ";

/// Parse a `request-too-large` message into a [`HeaderRefused`], or `None`
/// when the message is another limit (body, header set or URL size).
pub fn header_refused(message: &str) -> Option<HeaderRefused> {
    let rest = message.strip_prefix(HEADER_REFUSED_PREFIX)?;
    let (header, reason) = rest.split_once(": ")?;
    (!header.is_empty()).then(|| HeaderRefused { header: header.to_string(), reason: reason.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The platform's own refusal text, byte for byte (its egress tests pin
    /// the same strings): the format is a contract between the two.
    #[test]
    fn the_platforms_refusals_parse() {
        let r = header_refused(
            "header-refused: authorization: the platform sets Authorization on this request (secret-headers names it, or connection-auth is set); send your own Authorization or the platform's credential, not both",
        )
        .unwrap();
        assert_eq!(r.header, "authorization");
        assert!(r.reason.starts_with("the platform sets Authorization"));
        let r = header_refused("header-refused: x-boogy-caller: the x-boogy- prefix is the platform's; rename the header").unwrap();
        assert_eq!((r.header.as_str(), r.reason.as_str()), ("x-boogy-caller", "the x-boogy- prefix is the platform's; rename the header"));
    }

    #[test]
    fn another_limit_is_not_a_header_refusal() {
        assert_eq!(header_refused("request body exceeds max_request_bytes (1048576)"), None);
        assert_eq!(header_refused("header-refused: "), None);
    }
}
