//! JSON-RPC 2.0 envelope, error type, and response helpers.
//!
//! The codegen scaffolder emits a dispatcher route that:
//!   1. Parses the request body into [`Request`].
//!   2. Looks up `method` against a generated dispatch table.
//!   3. Decodes `params` into a typed struct, calls the handler, and
//!      converts `Result<T, RpcError>` into [`success_response`] or
//!      [`error_response`].
//!
//! Handler authors only see typed params + a `Result<T, RpcError>` return —
//! they don't write any envelope or HTTP plumbing.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::response;

/// A JSON-RPC request envelope.
///
/// `params` defaults to `Value::Null` if the request omits it. The dispatcher
/// is responsible for decoding it into the method-specific params struct.
#[derive(Debug, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub id: Option<Value>,
}

/// A typed JSON-RPC error returned from method handlers.
///
/// Standard codes (per the spec) live as constructors. Application-defined
/// errors should use positive codes via [`RpcError::application`].
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    /// Standard `-32700`: parse error.
    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self::new(-32700, msg)
    }

    /// Standard `-32600`: the request was malformed.
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self::new(-32600, msg)
    }

    /// Standard `-32601`: the method does not exist on this server.
    pub fn method_not_found(msg: impl Into<String>) -> Self {
        Self::new(-32601, msg)
    }

    /// Standard `-32602`: params didn't match the method's schema.
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self::new(-32602, msg)
    }

    /// Standard `-32603`: internal server error / unhandled state.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(-32603, msg)
    }

    /// Application-level error. Convention: positive codes.
    pub fn application(code: i64, msg: impl Into<String>) -> Self {
        Self::new(code, msg)
    }
}

impl From<String> for RpcError {
    fn from(s: String) -> Self {
        Self::internal(s)
    }
}

impl From<&str> for RpcError {
    fn from(s: &str) -> Self {
        Self::internal(s.to_string())
    }
}

/// Build a JSON-RPC success response.
pub fn success_response<T: Serialize>(id: Option<&Value>, result: &T) -> response::HttpResponse {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "result": serde_json::to_value(result).unwrap_or(Value::Null),
        "id": id,
    });
    response::raw(200, body.to_string().as_bytes(), "application/json")
}

/// Build a JSON-RPC error response.
pub fn error_response(id: Option<&Value>, error: &RpcError) -> response::HttpResponse {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "error": {
            "code": error.code,
            "message": &error.message,
        },
        "id": id,
    });
    response::raw(200, body.to_string().as_bytes(), "application/json")
}


/// Declarative JSON-RPC dispatcher.
///
/// Wraps the envelope-parse / params-decode / handler-call / response-build
/// loop so handlers can stay typed (`fn(P) -> Result<R, RpcError>` where P
/// is `Deserialize` and R is `Serialize`) and the user code stays small.
///
/// ```ignore
/// fn rpc_dispatch(req: &boogy_sdk::Request, _params: &Params) -> response::HttpResponse {
///     boogy_sdk::rpc::Dispatcher::new()
///         .method("search_notes", search_notes)
///         .method("share_note", share_note)
///         .handle(req)
/// }
/// ```
///
/// Error mapping:
/// - missing body → `-32600 invalid_request`
/// - body not parseable as a JSON-RPC envelope → `-32700 parse_error`
/// - method name not registered → `-32601 method_not_found`
/// - params don't match the handler's `P` type → `-32602 invalid_params`
/// - handler returned `Err(RpcError)` → that error, untouched
/// - serialising the handler's `Ok` value failed → `-32603 internal`
pub struct Dispatcher {
    handlers: Vec<(String, Box<dyn Fn(Value) -> Result<Value, RpcError>>)>,
    /// Captured method shapes — one entry per `.method()` registration,
    /// in registration order. Used by `Router::rpc` to populate
    /// `rpc_specs` at mount time and by the `rpc.discover` built-in.
    specs: Vec<crate::spec::MethodSpec>,
    /// Summary staged by `Dispatcher::summary`, applied to (and cleared by)
    /// the NEXT method registered. `None` between annotations.
    pending_summary: Option<String>,
    /// Description staged by `Dispatcher::description`, applied to (and
    /// cleared by) the NEXT method registered. `None` between annotations.
    pending_description: Option<String>,
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatcher {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            specs: Vec::new(),
            pending_summary: None,
            pending_description: None,
        }
    }

    /// Attach a one-line summary to the NEXT method registered. Flows into
    /// the generated openrpc.json (`summary`) so JSON-RPC clients + agents
    /// see what the method does.
    pub fn summary(mut self, s: impl Into<String>) -> Self {
        self.pending_summary = Some(s.into());
        self
    }

    /// Attach a longer description to the NEXT method registered (openrpc
    /// `description`).
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.pending_description = Some(d.into());
        self
    }

    /// Register a typed JSON-RPC method.
    ///
    /// `handler` is `fn(P) -> Result<R, RpcError>` (or any `Fn` of that shape).
    /// `P` is the params struct (must `Deserialize + JsonSchema`); `R` is
    /// the result struct (must `Serialize + JsonSchema`). The dispatcher
    /// decodes incoming `params` into `P`, serialises the returned `R`
    /// into the wire response, and records the method's shape for
    /// `rpc.discover` / `…/openrpc.json`.
    pub fn method<P, R, F>(mut self, name: &str, handler: F) -> Self
    where
        P: for<'de> serde::Deserialize<'de> + schemars::JsonSchema + 'static,
        R: serde::Serialize + schemars::JsonSchema + 'static,
        F: Fn(P) -> Result<R, RpcError> + 'static,
    {
        // Capture the method shape before type erasure so rpc.discover
        // and Router::rpc can serve the OpenRPC document without needing
        // to reconstruct P/R from the erased closure.
        self.specs.push(crate::spec::MethodSpec {
            name: name.to_string(),
            params_schema: crate::spec::schema_value::<P>(),
            result_schema: crate::spec::schema_value::<R>(),
            summary: self.pending_summary.take(),
            description: self.pending_description.take(),
        });

        let erased: Box<dyn Fn(Value) -> Result<Value, RpcError>> = Box::new(move |raw: Value| {
            let typed: P = serde_json::from_value(raw)
                .map_err(|e| RpcError::invalid_params(format!("{e}")))?;
            let result = handler(typed)?;
            serde_json::to_value(&result)
                .map_err(|e| RpcError::internal(format!("serialize result: {e}")))
        });
        self.handlers.push((name.to_string(), erased));
        self
    }

    /// The captured method shapes (one per `.method()` registration, in order).
    ///
    /// Called by `Router::rpc` at mount time to populate its `rpc_specs`
    /// table, and by the `rpc.discover` built-in to build the OpenRPC
    /// document in-protocol.
    pub fn method_specs(&self) -> &[crate::spec::MethodSpec] {
        &self.specs
    }

    /// Dispatch a request to the matching method handler.
    ///
    /// The whole envelope/error pipeline lives here so callers only see
    /// `req` in and `HttpResponse` out — no per-method boilerplate.
    ///
    /// The `rpc.discover` method is handled automatically (after all
    /// user-registered handlers are consulted, so a user-registered
    /// `rpc.discover` wins).
    pub fn handle(&self, req: &crate::Request) -> response::HttpResponse {
        let Some(body) = &req.body else {
            return error_response(None, &RpcError::invalid_request("missing body"));
        };
        let envelope: Request = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return error_response(None, &RpcError::parse_error(format!("{e}"))),
        };
        let id = envelope.id.as_ref();

        // User-registered handlers are consulted first so a user-registered
        // `rpc.discover` wins over the built-in.
        for (name, handler) in &self.handlers {
            if name == &envelope.method {
                return match handler(envelope.params.clone()) {
                    Ok(v) => success_response(id, &v),
                    Err(e) => error_response(id, &e),
                };
            }
        }

        // Built-in OpenRPC service-discovery method (the spec reserves
        // `rpc.*` as a namespace; this is the standard discovery hook).
        // OpenRPC service discovery (the spec reserves rpc.*). DocInfo
        // is the default here by necessity: at dispatch time only the
        // Dispatcher survives — the Router (and its doc_info) is gone.
        if envelope.method == "rpc.discover" {
            let doc = crate::spec::build_openrpc(&crate::spec::DocInfo::default(), &self.specs);
            return success_response(id, &doc);
        }

        error_response(id, &RpcError::method_not_found(envelope.method.clone()))
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct EchoParams { msg: String }
    #[derive(serde::Serialize, schemars::JsonSchema)]
    struct EchoResult { msg: String }

    fn echo(p: EchoParams) -> Result<EchoResult, RpcError> { Ok(EchoResult { msg: p.msg }) }

    fn rpc_req(body: &str) -> crate::Request {
        crate::Request {
            method: "POST".into(), path: "/rpc".into(), headers: vec![],
            body: Some(body.as_bytes().to_vec()), path_params: vec![], query_params: vec![],
        }
    }

    #[test]
    fn dispatcher_records_method_specs() {
        let d = Dispatcher::new().method("echo", echo);
        let specs = d.method_specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "echo");
        assert_eq!(specs[0].params_schema["properties"]["msg"]["type"], "string");
        assert_eq!(specs[0].result_schema["properties"]["msg"]["type"], "string");
    }

    #[test]
    fn summary_description_flow_into_openrpc_per_method() {
        let d = Dispatcher::new()
            .summary("Echo back")
            .description("Return the message unchanged.")
            .method("echo", echo)
            .method("echo2", echo);

        // Annotation lands on the first (annotated) method.
        let resp = d.handle(&rpc_req(r#"{"jsonrpc":"2.0","method":"rpc.discover","id":1}"#));
        let v: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        let m0 = &v["result"]["methods"][0];
        assert_eq!(m0["name"], "echo");
        assert_eq!(m0["summary"], "Echo back");
        assert_eq!(m0["description"], "Return the message unchanged.");

        // Per-method (cleared via take()): the next method has neither key.
        let m1 = &v["result"]["methods"][1];
        assert_eq!(m1["name"], "echo2");
        assert!(m1.get("summary").is_none(), "summary must not leak to the next method");
        assert!(m1.get("description").is_none(), "description must not leak to the next method");
    }

    #[test]
    fn rpc_discover_returns_openrpc_document() {
        let d = Dispatcher::new().method("echo", echo);
        let resp = d.handle(&rpc_req(r#"{"jsonrpc":"2.0","method":"rpc.discover","id":1}"#));
        let v: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert_eq!(v["result"]["openrpc"], "1.3.2");
        assert_eq!(v["result"]["methods"][0]["name"], "echo");
    }

    #[test]
    fn user_registered_discover_wins() {
        fn custom(_: serde_json::Value) -> Result<&'static str, RpcError> { Ok("custom") }
        let d = Dispatcher::new().method("rpc.discover", custom);
        let resp = d.handle(&rpc_req(r#"{"jsonrpc":"2.0","method":"rpc.discover","id":1}"#));
        let v: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert_eq!(v["result"], "custom");
    }
}
