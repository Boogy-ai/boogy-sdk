//! Finding another account's instance of a module: the platform's instance
//! lookup, `GET /v1/registry/instances/{handle}`, answered by the registry.
//!
//! A deployed service calls it with the `discovery::lookup` function that
//! `wit_glue!` generates (it needs `[capabilities] peer = true`). A lookup is a
//! point query — one handle, one module — and reports only instances whose
//! owner left them listed; an unlisted or absent instance reads the same, as
//! an empty list.

use serde::Deserialize;

/// The registry's workload, which answers lookups.
pub const REGISTRY: &str = "boogy://_sys/services/registry";

/// Which module to look for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Module {
    /// The module the calling service itself runs — how a module finds other
    /// instances of itself without naming its own address.
    Same,
    /// `<author>/<name>`.
    Named { author: String, name: String },
}

/// A route an instance publishes: the module's own path, which a peer call
/// reaches on any instance of the module, whatever path it is mounted at.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PublishedRoute {
    pub name: String,
    pub path: String,
    pub version: u32,
}

/// One listed instance.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Instance {
    /// `boogy://<owner>/services/<id>` — the target for a peer call.
    pub address: String,
    /// `boogy://<author>/modules/<name>`.
    pub module: String,
    pub module_version: String,
    /// Where it is served for a browser, when the platform can say.
    pub url: Option<String>,
    pub routes: Vec<PublishedRoute>,
}

impl Instance {
    /// The published route named `name`, if this instance publishes it.
    pub fn route(&self, name: &str) -> Option<&PublishedRoute> {
        self.routes.iter().find(|r| r.name == name)
    }
}

/// The registry path for a lookup of `handle`'s instances of `module`.
///
/// Every component is percent-encoded, so a value can only ever be itself: a
/// handle carrying `?`, `&`, `#`, `/` or `..` cannot change which module is
/// asked about, or reach another registry route.
pub fn lookup_path(handle: &str, module: &Module) -> String {
    let m = match module {
        Module::Same => "self".to_string(),
        Module::Named { author, name } => format!("{}/{}", encode(author), encode(name)),
    };
    format!("/v1/registry/instances/{}?module={m}", encode(handle))
}

/// Percent-encode everything but ASCII letters, digits, `-`, `_` and `~`
/// (`.` too, so `..` is never a path segment).
fn encode(component: &str) -> String {
    let mut out = String::with_capacity(component.len());
    for b in component.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Deserialize)]
struct Answer {
    instances: Vec<Instance>,
}

/// Parse the registry's answer to a lookup.
pub fn parse_answer(body: &[u8]) -> Result<Vec<Instance>, serde_json::Error> {
    serde_json::from_slice::<Answer>(body).map(|a| a.instances)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lookup_path_names_the_handle_and_the_module() {
        assert_eq!(lookup_path("dave", &Module::Same), "/v1/registry/instances/dave?module=self");
        assert_eq!(
            lookup_path("dave", &Module::Named { author: "tester".into(), name: "squad".into() }),
            "/v1/registry/instances/dave?module=tester/squad"
        );
    }

    /// A component can only ever be itself: a handle carrying `?`, `&`, `#`,
    /// `/` or `..` must not change which module is asked about, or reach
    /// another registry route.
    #[test]
    fn every_component_is_percent_encoded() {
        let p = lookup_path("dave?module=evil/x&", &Module::Same);
        assert_eq!(p, "/v1/registry/instances/dave%3Fmodule%3Devil%2Fx%26?module=self");
        assert_eq!(p.matches('?').count(), 1);
        let p = lookup_path("../search", &Module::Named { author: "a&b".into(), name: "n#/x".into() });
        assert_eq!(p, "/v1/registry/instances/%2E%2E%2Fsearch?module=a%26b/n%23%2Fx");
        assert_eq!(lookup_path("bob-smith_1.x~", &Module::Same), "/v1/registry/instances/bob-smith_1%2Ex~?module=self");
    }

    #[test]
    fn an_answer_parses_into_instances_and_their_routes() {
        let body = br#"{"instances":[{"address":"boogy://dave/services/squad","module":"boogy://tester/modules/squad","module_version":"0.1.24","url":null,"routes":[{"name":"peer-messages","path":"/chats/api/peer/messages","version":1}]}]}"#;
        let got = parse_answer(body).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].address, "boogy://dave/services/squad");
        assert_eq!(got[0].route("peer-messages").unwrap().version, 1);
        assert!(got[0].route("other").is_none());
        assert!(parse_answer(br#"{"instances":[]}"#).unwrap().is_empty());
    }
}
