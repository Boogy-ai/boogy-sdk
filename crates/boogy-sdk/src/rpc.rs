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
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatcher {
    pub fn new() -> Self {
        Self { handlers: Vec::new() }
    }

    /// Register a typed JSON-RPC method.
    ///
    /// `handler` is `fn(P) -> Result<R, RpcError>` (or any `Fn` of that shape).
    /// `P` is the params struct (must `Deserialize`); `R` is the result
    /// struct (must `Serialize`). The dispatcher decodes incoming `params`
    /// into `P` and serialises the returned `R` into the wire response.
    pub fn method<P, R, F>(mut self, name: &str, handler: F) -> Self
    where
        P: for<'de> serde::Deserialize<'de> + 'static,
        R: serde::Serialize + 'static,
        F: Fn(P) -> Result<R, RpcError> + 'static,
    {
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

    /// Dispatch a request to the matching method handler.
    ///
    /// The whole envelope/error pipeline lives here so callers only see
    /// `req` in and `HttpResponse` out — no per-method boilerplate.
    pub fn handle(&self, req: &crate::Request) -> response::HttpResponse {
        let Some(body) = &req.body else {
            return error_response(None, &RpcError::invalid_request("missing body"));
        };
        let envelope: Request = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return error_response(None, &RpcError::parse_error(format!("{e}"))),
        };
        let id = envelope.id.as_ref();

        for (name, handler) in &self.handlers {
            if name == &envelope.method {
                return match handler(envelope.params.clone()) {
                    Ok(v) => success_response(id, &v),
                    Err(e) => error_response(id, &e),
                };
            }
        }
        error_response(id, &RpcError::method_not_found(envelope.method.clone()))
    }
}
