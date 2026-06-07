//! Machine-readable API descriptions, captured at route-registration time.
//!
//! Each protocol surface is described in its standard format at its
//! standard name: OpenAPI 3.0.3 at `GET …/openapi.json` (REST),
//! OpenRPC 1.3.2 at `GET …/openrpc.json` + in-protocol `rpc.discover`
//! (JSON-RPC), and MCP's own `tools/list` discovery (with
//! `outputSchema`). Capture happens in the `describe()` hooks on
//! [`crate::response::IntoResponse`], [`crate::extract::FromRequest`],
//! and [`crate::router::IntoHandler`] — the registration moment is the
//! only point where extractor/response types are statically known.

use serde_json::{json, Value};

/// Document identity for generated specs. Set via [`crate::Router::info`];
/// the default is used when a service doesn't bother.
#[derive(Debug, Clone)]
pub struct DocInfo {
    pub title: String,
    pub version: String,
    pub description: Option<String>,
}

impl Default for DocInfo {
    fn default() -> Self {
        Self { title: "boogy-service".into(), version: "0.0.0".into(), description: None }
    }
}

/// JSON Schema for `T`, OpenAPI-3.0-flavored, fully inlined.
///
/// `inline_subschemas` keeps every schema self-contained — no
/// `definitions`/`$defs` pointers, so values can be dropped anywhere in
/// an OpenAPI or OpenRPC document without `$ref` rewriting.
pub fn schema_value<T: schemars::JsonSchema>() -> Value {
    let mut settings = schemars::gen::SchemaSettings::openapi3();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>().schema;
    serde_json::to_value(schema).unwrap_or_else(|_| json!({ "type": "object" }))
}

/// What one REST operation responds with on success.
#[derive(Debug, Clone, Default)]
pub struct ResponseSpec {
    pub status: u16,
    /// `None` for body-less responses (204/302) and undescribable bodies.
    pub schema: Option<Value>,
}

/// Everything captured about one REST route's request/response shape.
#[derive(Debug, Clone, Default)]
pub struct OperationSpec {
    /// JSON request-body schema (from a `Json<T>` extractor).
    pub request_body: Option<Value>,
    /// Query-string schema (from a `Query<T>` extractor; an object schema
    /// whose properties become individual query parameters).
    pub query: Option<Value>,
    /// Path-params schema (from a `Path<T>` extractor; object schema → named
    /// params, scalar schema → the single `{param}` in the route).
    pub path_params: Option<Value>,
    /// A `Principal` extractor was present → the operation requires auth.
    pub requires_principal: bool,
    /// Success response, if the return type was describable.
    pub response: Option<ResponseSpec>,
}

/// One registry entry. `Rest` entries come from typed registrations;
/// [`SpecEntry::Mcp`] / [`SpecEntry::Rpc`] entries from
/// [`crate::Router::mcp`] / [`crate::Router::rpc`].
#[derive(Debug, Clone)]
pub enum SpecEntry {
    Rest {
        method: String,
        path: String,
        op: OperationSpec,
        /// Set when the route was registered inside a `.group([guards], …)`
        /// — a guard wraps the handler independent of any `Principal`
        /// extractor. Either signal marks the operation `security`-required.
        guarded: bool,
    },
    /// An MCP dispatch endpoint. `guarded` mirrors the surrounding
    /// `.group()` guard state so the visibility filter knows whether the
    /// endpoint requires authentication.
    Mcp {
        path: String,
        /// `true` when the mount was registered inside a guarded group.
        guarded: bool,
    },
    /// A JSON-RPC dispatch endpoint. Same `guarded` semantics as [`Self::Mcp`].
    Rpc {
        path: String,
        /// `true` when the mount was registered inside a guarded group.
        guarded: bool,
    },
}

impl SpecEntry {
    /// Rewrite the entry's path (used by `Router::nest`).
    pub(crate) fn with_path(mut self, new_path: String) -> Self {
        match &mut self {
            SpecEntry::Rest { path, .. }
            | SpecEntry::Mcp { path, .. }
            | SpecEntry::Rpc { path, .. } => {
                *path = new_path;
            }
        }
        self
    }

    pub(crate) fn path(&self) -> &str {
        match self {
            SpecEntry::Rest { path, .. }
            | SpecEntry::Mcp { path, .. }
            | SpecEntry::Rpc { path, .. } => path,
        }
    }

    /// `true` when the entry requires an authenticated caller.
    pub(crate) fn is_guarded(&self) -> bool {
        match self {
            SpecEntry::Rest { guarded, op, .. } => *guarded || op.requires_principal,
            SpecEntry::Mcp { guarded, .. } | SpecEntry::Rpc { guarded, .. } => *guarded,
        }
    }
}

/// All spec entries registered on a Router.
#[derive(Debug, Clone, Default)]
pub struct SpecRegistry {
    pub entries: Vec<SpecEntry>,
}

/// One JSON-RPC method's captured shape (lives on `rpc::Dispatcher`).
#[derive(Debug, Clone)]
pub struct MethodSpec {
    pub name: String,
    pub params_schema: Value,
    pub result_schema: Value,
}

/// The RFC 7807 problem schema every SDK error path emits, advertised as
/// the `default` response on every REST operation.
fn problem_schema() -> Value {
    json!({
        "type": "object",
        "description": "RFC 7807 problem details (every SDK error path)",
        "properties": {
            "type":   { "type": "string" },
            "title":  { "type": "string" },
            "status": { "type": "integer" },
            "detail": { "type": "string" },
            "errors": { "type": "object", "additionalProperties": { "type": "array", "items": { "type": "string" } } }
        },
        "required": ["type", "title", "status"]
    })
}

/// Build the OpenAPI 3.0.3 document for a service's REST surface.
pub fn build_openapi(info: &DocInfo, reg: &SpecRegistry) -> Value {
    let mut paths = serde_json::Map::new();

    for entry in &reg.entries {
        match entry {
            SpecEntry::Rest { method, path, op, guarded } => {
                let mut operation = serde_json::Map::new();

                // Parameters: named path params + query properties.
                let mut params = Vec::new();
                if let Some(pp) = &op.path_params {
                    push_object_params(&mut params, pp, "path", path);
                }
                if let Some(q) = &op.query {
                    push_object_params(&mut params, q, "query", path);
                }
                if !params.is_empty() {
                    operation.insert("parameters".into(), Value::Array(params));
                }

                if let Some(body) = &op.request_body {
                    operation.insert("requestBody".into(), json!({
                        "required": true,
                        "content": { "application/json": { "schema": body } }
                    }));
                }

                let mut responses = serde_json::Map::new();
                if let Some(resp) = &op.response {
                    let status = resp.status.to_string();
                    let entry = match &resp.schema {
                        Some(s) => json!({ "description": "Success",
                            "content": { "application/json": { "schema": s } } }),
                        None => json!({ "description": "Success" }),
                    };
                    responses.insert(status, entry);
                } else {
                    responses.insert("200".into(), json!({ "description": "Success" }));
                }
                responses.insert("default".into(), json!({
                    "description": "Error",
                    "content": { "application/problem+json": { "schema": problem_schema() } }
                }));
                operation.insert("responses".into(), Value::Object(responses));

                if *guarded || op.requires_principal {
                    operation.insert("security".into(), json!([{ "boogyAuth": [] }]));
                }

                paths.entry(openapi_path(path))
                    .or_insert_with(|| Value::Object(serde_json::Map::new()))
                    .as_object_mut()
                    .unwrap_or_else(|| unreachable!("entry value inserted by or_insert_with above is always an Object"))
                    .insert(method.to_lowercase(), Value::Object(operation));
            }
            SpecEntry::Mcp { path, .. } => {
                insert_protocol_stub(&mut paths, path,
                    "MCP (Model Context Protocol) endpoint. Discover capabilities in-protocol: \
                     POST a JSON-RPC `tools/list` / `resources/list` / `prompts/list` request.");
            }
            SpecEntry::Rpc { path, .. } => {
                insert_protocol_stub(&mut paths, path,
                    "JSON-RPC 2.0 endpoint. Method catalog: GET the sibling `openrpc.json` \
                     document, or call the in-protocol `rpc.discover` method.");
            }
        }
    }

    json!({
        "openapi": "3.0.3",
        "info": {
            "title": info.title,
            "version": info.version,
            "description": info.description,
        },
        "paths": paths,
        "components": {
            "securitySchemes": {
                "boogyAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "description": "Boogy platform token (PASETO v4.public) or service-issued sk_* API key"
                }
            }
        }
    })
}

/// Flatten an object schema's properties into OpenAPI parameter objects.
/// Scalar (non-object) Path schemas map to the single `{name}` in the route.
fn push_object_params(out: &mut Vec<Value>, schema: &Value, location: &str, route_path: &str) {
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        let required: Vec<&str> = schema.get("required")
            .and_then(|r| r.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        for (name, prop) in props {
            out.push(json!({
                "name": name,
                "in": location,
                "required": location == "path" || required.contains(&name.as_str()),
                "schema": prop,
            }));
        }
    } else if location == "path" {
        // Scalar Path<T> — bind to the route's single `{param}` segment.
        if let Some(name) = single_path_param(route_path) {
            out.push(json!({ "name": name, "in": "path", "required": true, "schema": schema }));
        }
    }
}

/// `/api/notes/{id}` → `Some("id")`; multi-param or no-param paths → None.
fn single_path_param(path: &str) -> Option<String> {
    let mut params = path.split('/')
        .filter(|s| s.starts_with('{') && s.ends_with('}'))
        .map(|s| s[1..s.len() - 1].trim_start_matches('*').to_string());
    match (params.next(), params.next()) {
        (Some(p), None) => Some(p),
        _ => None,
    }
}

/// matchit catch-all `{*rest}` → OpenAPI-legal `{rest}`.
fn openapi_path(path: &str) -> String {
    path.replace("{*", "{")
}

fn insert_protocol_stub(paths: &mut serde_json::Map<String, Value>, path: &str, description: &str) {
    paths.entry(openapi_path(path))
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .unwrap_or_else(|| unreachable!("entry value inserted by or_insert_with above is always an Object"))
        .insert("post".into(), json!({
            "description": description,
            "responses": { "200": { "description": "JSON-RPC response envelope" } }
        }));
}

/// Build the OpenRPC 1.3.2 document for a Dispatcher's method table.
pub fn build_openrpc(info: &DocInfo, methods: &[MethodSpec]) -> Value {
    json!({
        "openrpc": "1.3.2",
        "info": {
            "title": info.title,
            "version": info.version,
            "description": info.description,
        },
        "methods": methods.iter().map(|m| json!({
            "name": m.name,
            "params": [{
                "name": "params",
                "required": true,
                "schema": m.params_schema,
            }],
            "result": { "name": "result", "schema": m.result_schema },
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct CreateNote { title: String, body: Option<String> }

    // ─── Hardening sweep (spec-endpoints) ─────────────────────────────────

    /// Test 7: query params required logic — "a" (required), "b" (optional);
    /// path params are always required regardless of the schema's `required` list.
    #[test]
    fn query_params_required_logic() {
        // Build a query schema with one required field and one optional field.
        let query_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "a": { "type": "string" },
                "b": { "type": "integer" }
            },
            "required": ["a"]
        });
        // Build a path-param schema as an object (two-param path → named params).
        let path_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer" }
            }
        });

        let mut params = Vec::new();
        push_object_params(&mut params, &path_schema, "path", "/notes/{id}");
        push_object_params(&mut params, &query_schema, "query", "/notes/{id}");

        let find = |name: &str, location: &str| {
            params.iter().find(|p| p["name"] == name && p["in"] == location).cloned()
        };

        // Path param: always required.
        let id_param = find("id", "path").expect("path param 'id' must be present");
        assert_eq!(id_param["required"], serde_json::json!(true),
            "path params are always required");

        // Query "a": listed in required → required = true.
        let a_param = find("a", "query").expect("query param 'a' must be present");
        assert_eq!(a_param["required"], serde_json::json!(true),
            "query param 'a' is in the required list → required=true");

        // Query "b": not in required → required = false.
        let b_param = find("b", "query").expect("query param 'b' must be present");
        assert_eq!(b_param["required"], serde_json::json!(false),
            "query param 'b' is not in the required list → required=false");
    }

    /// Test 8: scalar path param binds to the single route param on a
    /// one-param path, but emits no parameter on a two-param path
    /// (ambiguous binding).
    #[test]
    fn scalar_path_param_binds_to_single_route_param() {
        // Scalar integer schema (no "properties" → scalar branch).
        let scalar = serde_json::json!({ "type": "integer" });

        // One-param path: scalar binds to "id".
        let mut params_one = Vec::new();
        push_object_params(&mut params_one, &scalar, "path", "/notes/{id}");
        assert_eq!(params_one.len(), 1, "scalar on one-param path must yield one parameter");
        assert_eq!(params_one[0]["name"], "id");
        assert_eq!(params_one[0]["in"], "path");
        assert_eq!(params_one[0]["required"], serde_json::json!(true));
        assert_eq!(params_one[0]["schema"]["type"], "integer");

        // Two-param path: scalar can't bind ambiguously → no parameters emitted.
        let mut params_two = Vec::new();
        push_object_params(&mut params_two, &scalar, "path", "/a/{x}/{y}");
        assert!(params_two.is_empty(),
            "scalar path param on a two-segment route must emit no parameters (ambiguous)");
    }

    /// Test 9: path containing a catch-all `{*path}` is converted to the
    /// OpenAPI-legal `{path}` in the document key.
    #[test]
    fn catch_all_path_converted() {
        let mut reg = SpecRegistry::default();
        reg.entries.push(SpecEntry::Rest {
            method: "GET".into(),
            path: "/files/{*path}".into(),
            op: OperationSpec::default(),
            guarded: false,
        });
        let doc = build_openapi(&DocInfo::default(), &reg);
        // The matchit catch-all must be converted to OpenAPI's `{path}`.
        assert!(doc["paths"]["/files/{path}"]["get"].is_object(),
            "catch-all {{*path}} must be converted to {{path}} in the doc key; doc: {doc}");
        // The raw matchit form must NOT appear as a key.
        assert!(doc["paths"].get("/files/{*path}").map_or(true, |v| v.is_null()),
            "raw {{*path}} must not appear in the OpenAPI paths");
    }

    #[test]
    fn openapi_doc_basic_shape() {
        let mut reg = SpecRegistry::default();
        let mut op = OperationSpec::default();
        op.request_body = Some(schema_value::<CreateNote>());
        op.response = Some(ResponseSpec { status: 201, schema: Some(schema_value::<CreateNote>()) });
        reg.entries.push(SpecEntry::Rest { method: "POST".into(), path: "/api/notes".into(), op, guarded: true });
        reg.entries.push(SpecEntry::Mcp { path: "/mcp".into(), guarded: false });

        let doc = build_openapi(&DocInfo::default(), &reg);
        assert_eq!(doc["openapi"], "3.0.3");
        let post = &doc["paths"]["/api/notes"]["post"];
        assert!(post["requestBody"]["content"]["application/json"]["schema"].is_object());
        assert!(post["responses"]["201"].is_object());
        // every REST op advertises the RFC 7807 default error
        assert!(post["responses"]["default"]["content"]["application/problem+json"].is_object());
        // guarded routes carry a security requirement
        assert!(post["security"].is_array());
        // MCP mount listed as an undescribed POST pointing at in-protocol discovery
        let mcp = &doc["paths"]["/mcp"]["post"];
        assert!(mcp["description"].as_str().unwrap().contains("tools/list"));
    }

    #[test]
    fn schemas_are_inlined_openapi3_flavor() {
        // SchemaSettings::openapi3 with inline_subschemas — no $defs/definitions
        // pointers that OpenAPI 3.0 tooling can't resolve.
        let v = schema_value::<CreateNote>();
        assert!(v.get("$defs").is_none() && v.get("definitions").is_none());
        assert_eq!(v["type"], "object");
    }

    #[test]
    fn openrpc_doc_basic_shape() {
        let methods = vec![MethodSpec {
            name: "search_notes".into(),
            params_schema: schema_value::<CreateNote>(),
            result_schema: schema_value::<CreateNote>(),
        }];
        let doc = build_openrpc(&DocInfo::default(), &methods);
        assert_eq!(doc["openrpc"], "1.3.2");
        assert_eq!(doc["methods"][0]["name"], "search_notes");
        assert_eq!(doc["methods"][0]["params"][0]["name"], "params");
        assert!(doc["methods"][0]["result"]["schema"].is_object());
    }
}
