//! The CLI's HTTP clients. TLS is rustls on the `ring` provider, installed
//! here before any client is built: reqwest is built without a provider of
//! its own, and a client built with none installed panics.

/// A reqwest client builder whose TLS runs on `ring`.
pub fn client_builder() -> reqwest::ClientBuilder {
    install_ring();
    reqwest::Client::builder()
}

/// Make `ring` rustls's process-wide provider, once. A provider installed
/// already is kept.
fn install_ring() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// [`client_builder`] with reqwest's defaults.
pub fn client() -> reqwest::Client {
    client_builder().build().expect("a reqwest client with the ring provider")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client builds, and the provider it got is `ring` (the `Debug` of
    /// the randomness source names it: `Ring`, or `AwsLcRs` for aws-lc-rs).
    #[test]
    fn a_client_builds_on_ring() {
        client();
        let provider = rustls::crypto::CryptoProvider::get_default().expect("a provider is installed");
        assert_eq!(format!("{:?}", provider.secure_random), "Ring");
    }
}
