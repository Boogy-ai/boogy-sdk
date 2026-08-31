//! File and static-asset storage.
//!
//! **Your component never carries file bytes.** You create an upload ticket
//! and hand it to the client; the client sends the bytes straight to a
//! host-owned route; the host serves them back. No request that reads a file
//! ever runs your code, which is why a file costs no compute and no instance
//! slot — and why an asset-heavy app is cheap here.
//!
//! ```ignore_snippet: an author-facing sketch — the handler, its DTO and its error type are not in scope in this block
//! // 1. Mint a ticket. Return it to the client as JSON.
//! //    `files_*` are emitted at your crate level by `wit_glue!` — no import.
//! let ticket = files_create_upload("avatars",
//!     Upload::new().content_type("image/png").size_hint(len))?;
//!
//! // 2. Show the file. Mint the URL AT RENDER TIME — never store it.
//! let src = files_url("avatars", &key, None)?;
//! ```
//!
//! # The one mistake to avoid
//!
//! **Never store the result of [`url`] in your database.** For a non-public
//! collection it carries a short-lived grant and will stop working; even for
//! a public collection, storing it bakes in an origin that a custom domain or
//! a rename invalidates. Store the collection and key — or a
//! [`FileRef`](crate::files::FileRef), which is that pair and nothing else —
//! and call `url()` when you render.
//!
//! # Capability gate
//!
//! `[capabilities] files = true`, plus a `[[files.collections]]` block per
//! collection. An undeclared collection fails closed with
//! [`FilesError::UnknownCollection`].

use std::fmt;

/// Options for [`create_upload`].
///
/// The default is the safe one: omitting `key` lets the host mint a
/// collision-free, non-enumerable key that cannot traverse. Supply your own
/// only when you need a stable, meaningful name.
#[derive(Debug, Clone, Default)]
pub struct Upload {
    pub key: Option<String>,
    pub content_type: Option<String>,
    pub owner: Option<String>,
    pub ttl_seconds: Option<u32>,
    pub size_hint: Option<u64>,
}

impl Upload {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a specific key instead of a host-minted one.
    pub fn key(mut self, k: impl Into<String>) -> Self {
        self.key = Some(k.into());
        self
    }

    pub fn content_type(mut self, ct: impl Into<String>) -> Self {
        self.content_type = Some(ct.into());
        self
    }

    /// Attribute the file to a principal other than the caller.
    pub fn owner(mut self, o: impl Into<String>) -> Self {
        self.owner = Some(o.into());
        self
    }

    pub fn ttl_seconds(mut self, t: u32) -> Self {
        self.ttl_seconds = Some(t);
        self
    }

    /// Declare the size up front.
    ///
    /// Worth passing: it lets the host reject an oversized upload at ticket
    /// time rather than after the client has sent the bytes, and it is what
    /// selects the direct-to-storage transport for large files.
    pub fn size_hint(mut self, n: u64) -> Self {
        self.size_hint = Some(n);
        self
    }
}

/// What the platform knows about one stored file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub key: String,
    pub collection: String,
    pub size: u64,
    pub content_type: String,
    pub owner: Option<String>,
    pub created_at_millis: u64,
    /// `false` while an upload ticket is outstanding. A file that is not
    /// ready does not serve.
    pub ready: bool,
}

/// Everything a client needs to send the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadTicket {
    /// **Opaque.** Either a host route or a presigned storage URL — the host
    /// chooses. Pass it to your client unmodified; do not parse it, do not
    /// rewrite it, and do not store it.
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub key: String,
    pub expires_at_millis: u64,
}

/// One bounded page of a collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePage {
    pub files: Vec<FileInfo>,
    pub next_cursor: Option<String>,
}

/// Why a files call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesError {
    /// `[capabilities] files` is not granted.
    CapabilityDenied,
    /// No such `[[files.collections]]` block.
    UnknownCollection(String),
    NotFound,
    /// Carries the limit that was exceeded.
    TooLarge(u64),
    UnsupportedContentType(String),
    QuotaExceeded,
    /// The bytes have not arrived yet.
    NotReady,
    InvalidKey(String),
    RateLimited,
    /// Attempted inside an open transaction. A transaction body is
    /// re-runnable, so it may hold no irreversible external effect — the same
    /// rule that denies outbound HTTP and signing writes there. Move the call
    /// outside the transaction. It does **not** poison the transaction.
    DeniedInTransaction,
    Internal(String),
}

impl FilesError {
    /// The HTTP status this maps to.
    pub fn status(&self) -> u16 {
        match self {
            FilesError::CapabilityDenied => 403,
            FilesError::UnknownCollection(_) => 404,
            FilesError::NotFound => 404,
            FilesError::TooLarge(_) => 413,
            FilesError::UnsupportedContentType(_) => 415,
            // 507 Insufficient Storage: the request is well-formed and
            // permitted, and the only thing wrong is that there is no room.
            FilesError::QuotaExceeded => 507,
            FilesError::NotReady => 409,
            FilesError::InvalidKey(_) => 400,
            FilesError::RateLimited => 429,
            FilesError::DeniedInTransaction => 409,
            FilesError::Internal(_) => 500,
        }
    }
}

impl fmt::Display for FilesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilesError::CapabilityDenied => {
                write!(f, "files capability not granted; add `files = true` to [capabilities]")
            }
            FilesError::UnknownCollection(c) => {
                write!(f, "no [[files.collections]] block named {c:?}")
            }
            FilesError::NotFound => write!(f, "file not found"),
            FilesError::TooLarge(n) => write!(f, "file exceeds the {n}-byte limit"),
            FilesError::UnsupportedContentType(c) => {
                write!(f, "content type {c:?} is not accepted by this collection")
            }
            FilesError::QuotaExceeded => write!(f, "storage quota exceeded"),
            FilesError::NotReady => write!(f, "file upload has not completed"),
            FilesError::InvalidKey(m) => write!(f, "invalid file key: {m}"),
            FilesError::RateLimited => write!(f, "rate limited"),
            FilesError::DeniedInTransaction => write!(
                f,
                "files writes are not allowed inside a transaction; move the call outside tx()"
            ),
            FilesError::Internal(m) => write!(f, "internal error: {m}"),
        }
    }
}

impl std::error::Error for FilesError {}

/// A stored reference to a file: collection and key, and nothing else.
///
/// This type exists to make the most common object-storage mistake
/// unrepresentable. Storing a URL in your database bakes in an expiry (for a
/// grant URL) or an origin (for a public one); storing a `FileRef` stores the
/// identity, and you mint the URL when you render.
///
/// ```ignore_snippet: a model + render sketch — the derive's surrounding schema and the handler's error type are not in scope in this block
/// #[derive(Model)]
/// struct Profile {
///     id: String,
///     avatar: Option<FileRef>,
/// }
///
/// let src = profile.avatar.as_ref().map(|f| f.url(None)).transpose()?;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    pub collection: String,
    pub key: String,
}

impl FileRef {
    pub fn new(collection: impl Into<String>, key: impl Into<String>) -> Self {
        Self { collection: collection.into(), key: key.into() }
    }

    /// The stored form: `collection:key`.
    ///
    /// Split on the FIRST colon only, because a key may legitimately contain
    /// one — and may contain `/` — while a collection name may contain
    /// neither.
    pub fn to_column_value(&self) -> String {
        format!("{}:{}", self.collection, self.key)
    }

    pub fn from_column_value(s: &str) -> Result<Self, FilesError> {
        let (collection, key) = s
            .split_once(':')
            .ok_or_else(|| FilesError::InvalidKey(format!("malformed file ref {s:?}")))?;
        if collection.is_empty() || key.is_empty() {
            return Err(FilesError::InvalidKey(format!("malformed file ref {s:?}")));
        }
        Ok(Self { collection: collection.to_string(), key: key.to_string() })
    }
}

/// A `FileRef` is a stored column, encoded as `"collection:key"` text.
///
/// **This impl is what makes the type useful.** `FileRef` exists to be a model
/// field — its whole purpose is to occupy the slot where an author would
/// otherwise store a URL — so without `Field` it is a type nobody can put in a
/// `#[derive(Model)]` struct, and the guidance to use it would not compile.
///
/// Decoding is infallible, matching every other `Field`: a missing or
/// malformed value yields an empty ref rather than panicking on read. A caller
/// that needs to distinguish "absent" from "malformed" uses
/// [`FileRef::from_column_value`], which returns a `Result`.
impl crate::model::Field for FileRef {
    fn col_type() -> crate::store::ColType {
        crate::store::ColType::Text
    }
    fn to_val(&self) -> crate::store::Val {
        crate::store::Val::Text(self.to_column_value())
    }
    fn from_val(v: &crate::store::Val) -> Self {
        FileRef::from_column_value(&v.as_text()).unwrap_or_else(|_| FileRef {
            collection: String::new(),
            key: String::new(),
        })
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_builder_defaults_to_letting_the_host_mint_the_key() {
        let u = Upload::new();
        assert!(u.key.is_none(), "the default must be the safe one");
        assert!(u.content_type.is_none());
        assert!(u.size_hint.is_none());
    }

    #[test]
    fn upload_builder_threads_every_option() {
        let u = Upload::new()
            .key("a/b.png")
            .content_type("image/png")
            .owner("agent_1")
            .ttl_seconds(60)
            .size_hint(1234);
        assert_eq!(u.key.as_deref(), Some("a/b.png"));
        assert_eq!(u.content_type.as_deref(), Some("image/png"));
        assert_eq!(u.owner.as_deref(), Some("agent_1"));
        assert_eq!(u.ttl_seconds, Some(60));
        assert_eq!(u.size_hint, Some(1234));
    }

    #[test]
    fn files_error_renders_a_status_a_handler_can_return() {
        assert_eq!(FilesError::NotFound.status(), 404);
        assert_eq!(FilesError::TooLarge(10).status(), 413);
        assert_eq!(FilesError::UnsupportedContentType("x".into()).status(), 415);
        assert_eq!(FilesError::QuotaExceeded.status(), 507);
        assert_eq!(FilesError::RateLimited.status(), 429);
        assert_eq!(FilesError::CapabilityDenied.status(), 403);
        assert_eq!(FilesError::DeniedInTransaction.status(), 409);
        assert_eq!(FilesError::Internal("x".into()).status(), 500);
    }

    #[test]
    fn every_error_says_what_to_do_about_it() {
        // A message that only names the failure leaves the author guessing.
        let denied = FilesError::CapabilityDenied.to_string();
        assert!(denied.contains("[capabilities]"), "{denied}");
        let tx = FilesError::DeniedInTransaction.to_string();
        assert!(tx.contains("outside tx()"), "{tx}");
    }

    #[test]
    fn a_file_ref_stores_a_reference_not_a_url() {
        let r = FileRef::new("avatars", "abc.png");
        let stored = r.to_column_value();
        assert!(!stored.contains("http"), "a FileRef must never serialize a URL: {stored}");
        assert_eq!(FileRef::from_column_value(&stored).unwrap(), r);
    }

    #[test]
    fn a_file_ref_round_trips_a_key_containing_separators() {
        for key in ["user1/2026/report.pdf", "a:b", "x/y:z/w"] {
            let r = FileRef::new("docs", key);
            assert_eq!(FileRef::from_column_value(&r.to_column_value()).unwrap(), r);
        }
    }

    #[test]
    fn a_file_ref_round_trips_as_a_model_field() {
        // Without a `Field` impl, `Option<FileRef>` cannot be a model column
        // at all — which would make the documented usage fail to compile. The
        // mirror's snippet gate caught exactly that.
        use crate::model::Field as _;
        use crate::store::{ColType, Val};
        assert!(matches!(<FileRef as crate::model::Field>::col_type(), ColType::Text));

        let r = FileRef::new("docs", "a/b:c.pdf");
        let v = r.to_val();
        assert_eq!(<FileRef as crate::model::Field>::from_val(&v), r);

        // Infallible on read, like every other Field.
        let junk = Val::Text("not-a-ref".to_string());
        assert_eq!(<FileRef as crate::model::Field>::from_val(&junk).key, "");
    }

    #[test]
    fn a_malformed_column_value_is_an_error_not_a_panic() {
        for bad in ["", "nocolon", ":", "coll:", ":key"] {
            assert!(FileRef::from_column_value(bad).is_err(), "{bad:?}");
        }
    }
}

#[cfg(test)]
mod error_conversion_tests {
    use super::*;
    use crate::error::ApiError;

    #[test]
    fn a_caller_fixable_error_keeps_its_status() {
        // These are the client's problem and say so.
        assert_eq!(ApiError::from(FilesError::NotFound).status, 404);
        assert_eq!(ApiError::from(FilesError::TooLarge(10)).status, 413);
        assert_eq!(ApiError::from(FilesError::QuotaExceeded).status, 507);
        assert_eq!(ApiError::from(FilesError::RateLimited).status, 429);
    }

    #[test]
    fn a_service_misconfiguration_becomes_a_500_not_a_403() {
        // A client can do nothing about a manifest that does not grant the
        // capability. Returning 403 would send them hunting for credentials
        // they do not need; this is the service's bug, so it reads as one.
        let denied = ApiError::from(FilesError::CapabilityDenied);
        assert_eq!(denied.status, 500);
        assert!(!denied.detail.unwrap_or_default().contains("[capabilities]"),
            "the manifest hint belongs in the log, not the client's response");

        assert_eq!(ApiError::from(FilesError::UnknownCollection("x".into())).status, 500);
    }

    #[test]
    fn an_internal_error_does_not_leak_its_detail_to_the_client() {
        // The detail names infrastructure the caller must never see. The
        // literal is deliberately generic: this crate is public, so a real
        // backend name here would leak the platform's internals into the
        // published SDK — which is what the mirror's privacy gate refuses.
        let e = ApiError::from(FilesError::Internal(
            "store backend refused connection at 10.0.0.4".into(),
        ));
        assert_eq!(e.status, 500);
        assert!(!e.detail.unwrap_or_default().contains("10.0.0.4"));
    }
}
