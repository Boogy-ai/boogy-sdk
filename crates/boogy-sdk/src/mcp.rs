//! Model Context Protocol (MCP) server.
//!
//! MCP exposes tools, resources, and prompts to LLM clients over
//! JSON-RPC 2.0. This module is the Boogy-side server: register
//! tools, return an `HttpResponse` for incoming JSON-RPC requests, and
//! the wire envelope / handshake / error mapping is taken care of.
//!
//! Implements the `initialize` handshake plus tools, resources, and
//! prompts. SSE streaming is deferred — responses are buffered JSON.
//!
//! # Authoring shape
//!
//! Prefer `tool_typed::<P, R>` — typed argument struct deriving
//! `Deserialize + JsonSchema`, typed result deriving `Serialize`,
//! handler returns `Result<R, ApiError>`. The advertised
//! `inputSchema` is auto-derived from `P`'s `JsonSchema` impl, so
//! the deserializer and the protocol surface can't drift.
//!
//! ```ignore
//! use schemars::JsonSchema;
//! use boogy_sdk::mcp::{McpServer, tool};
//!
//! #[derive(Deserialize, JsonSchema)]
//! struct CreateNoteArgs { title: String, body: String }
//!
//! #[derive(Serialize)]
//! struct NoteOut { id: String, title: String }
//!
//! fn mcp_dispatch(req: &mut Req<'_>) -> response::HttpResponse {
//!     McpServer::new("notes-mcp", "0.1.0")
//!         .tool_typed(
//!             tool("create_note").description("Create a note for the authenticated agent."),
//!             create_note_tool,
//!         )
//!         .handle(req.request)
//! }
//!
//! fn create_note_tool(args: CreateNoteArgs) -> Result<NoteOut, ApiError> {
//!     let principal = auth::current_principal().ok_or_else(ApiError::unauthenticated)?;
//!     // ... persist + return ...
//! }
//! ```
//!
//! The raw `tool(descriptor, |Value| -> Result<ToolResult, RpcError>)`
//! registration stays as the escape hatch for tools that need full
//! `ToolResult` control (multi-content-block responses, marked errors).
//!
//! # Auth integration
//!
//! Tool handlers use the `auth` capability the same way REST handlers
//! do — call `auth::current_principal()` from inside the handler.
//! That helper unifies PASETO sessions and `sk_*` API keys (the
//! api_keys guard stashes the resolved principal in the SDK's
//! per-request slot, which `current_principal()` consults). The
//! host's auth middleware has already verified the bearer (PASETO or
//! API key) before the request reaches wasm; this module doesn't
//! re-verify.
//!
//! # Error mapping
//!
//! Two distinct error channels — don't confuse them:
//!
//! - **JSON-RPC errors** ([`RpcError`]): protocol failures (parse error,
//!   method not found, invalid params shape). Returned by [`McpServer::handle`]
//!   as the JSON-RPC `error` field.
//! - **Tool-reported errors**: the tool ran but produced a failure the
//!   client should see. Return `ToolResult::error("...")` — that's an
//!   *Ok* JSON-RPC response carrying `isError: true`.
//!
//! Returning `Err(RpcError)` from a tool handler currently surfaces as
//! a JSON-RPC error. If you want the client to see a tool-level error
//! (with the model in the loop), use `ToolResult::error` and return `Ok`.

use std::time::Instant;

use serde::Serialize;
use serde_json::Value;

use crate::response;
use crate::rpc::{self, RpcError};

/// Response header set by [`McpServer::handle`] so the host can
/// attribute an `mcp_invocation` telemetry event for each call.
/// Header value is JSON: `{"k":"<kind>","n":"<name>","us":<wall_micros>,"o":"<outcome>"}`
/// where:
///   * `k` is `tool` / `resource` / `resource_template` / `prompt`,
///   * `n` is the tool name / resource URI / prompt name,
///   * `us` is wall time in microseconds,
///   * `o` is `success` / `client_error` / `server_error`.
///
/// The host (`boogy-host`) reads this header in `dispatch_inner`
/// and emits the corresponding event. Keep in sync with the `MCP_TELEMETRY_HEADER`
/// constant referenced there.
pub const MCP_TELEMETRY_HEADER: &str = "x-boogy-mcp";

/// Map a JSON-RPC error code to a telemetry outcome string. Standard
/// JSON-RPC client-fault codes (`-32700`, `-32600`, `-32601`, `-32602`)
/// are `client_error`; everything else (including the standard
/// `internal_error` code and application codes) is `server_error`.
fn outcome_for_rpc_code(code: i64) -> &'static str {
    match code {
        -32700 | -32600 | -32601 | -32602 => "client_error",
        _ => "server_error",
    }
}

/// Append the telemetry header to an HTTP response. JSON-encodes the
/// kind/name/timing/outcome payload. `name` is escaped for `"` and `\`
/// since resource URIs can contain colons / slashes (no quotes in
/// practice but we keep the escape robust).
fn inject_mcp_telemetry(
    mut resp: response::HttpResponse,
    kind: &str,
    name: &str,
    start: Instant,
    outcome: &str,
) -> response::HttpResponse {
    let micros = start.elapsed().as_micros() as u64;
    let escaped_name = name.replace('\\', "\\\\").replace('"', "\\\"");
    let value = format!(
        r#"{{"k":"{kind}","n":"{escaped_name}","us":{micros},"o":"{outcome}"}}"#
    );
    resp.headers
        .push((MCP_TELEMETRY_HEADER.to_string(), value));
    resp
}

/// MCP protocol version this server speaks. Negotiated during the
/// `initialize` handshake; if a client requests a different version
/// we still echo this one back — the client decides whether to proceed.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Type-erased tool handler. Takes the validated JSON-RPC `arguments`
/// payload, returns either a `ToolResult` (success or tool-reported
/// error) or an `RpcError` (protocol-level rejection). Most handlers
/// will just call `serde_json::from_value` on `args` to get typed
/// inputs and map deserialization failures to `RpcError::invalid_params`.
pub type ToolHandler = Box<dyn Fn(Value) -> Result<ToolResult, RpcError>>;

/// Resource handler. Receives the resolved URI (with template
/// variables filled in for template resources), returns the content
/// blocks. Multiple blocks per URI are allowed for multi-part
/// resources but a single Text block is the common case.
pub type ResourceHandler = Box<dyn Fn(&str) -> Result<Vec<ResourceContent>, RpcError>>;

/// Prompt handler. Receives the validated `arguments` map, returns
/// the rendered messages. Argument validation against
/// `Prompt::arguments[].required` happens in the SDK before this
/// runs — a missing required arg is reported as `invalid_params`.
pub type PromptHandler =
    Box<dyn Fn(std::collections::HashMap<String, String>) -> Result<PromptResult, RpcError>>;

/// One registered tool: descriptor + handler.
struct ToolEntry {
    descriptor: Tool,
    handler: ToolHandler,
}

/// One registered static resource (concrete URI).
struct ResourceEntry {
    descriptor: Resource,
    handler: ResourceHandler,
}

/// One registered resource template (URI pattern with variables).
struct ResourceTemplateEntry {
    descriptor: ResourceTemplate,
    compiled: CompiledTemplate,
    handler: ResourceHandler,
}

struct PromptEntry {
    descriptor: Prompt,
    handler: PromptHandler,
}

/// Tool descriptor advertised in `tools/list`. Build via [`tool`].
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the `arguments` shape. Clients use it
    /// to validate inputs before sending; the handler should still
    /// validate defensively.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// The placeholder input-schema [`tool`] returns when the caller
/// hasn't customized one yet. Kept as a single source of truth so
/// [`McpServer::tool_typed`] can detect "default, replace with derived"
/// vs. "user-provided, leave alone."
fn default_tool_input_schema() -> Value {
    serde_json::json!({ "type": "object" })
}

/// Builder for [`Tool`]. `name` is required; everything else is optional.
pub fn tool(name: impl Into<String>) -> Tool {
    Tool {
        name: name.into(),
        description: None,
        input_schema: default_tool_input_schema(),
    }
}

impl Tool {
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    pub fn input_schema(mut self, schema: Value) -> Self {
        self.input_schema = schema;
        self
    }
}

/// Result of a tool invocation. Either contentful success or a
/// tool-reported failure (`is_error = true`). Both serialize as a
/// successful JSON-RPC response.
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub content: Vec<Content>,
    /// `isError: true` tells the LLM client the tool reported a failure.
    /// Distinct from a JSON-RPC error — see module docs.
    #[serde(rename = "isError", skip_serializing_if = "is_false")]
    pub is_error: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl ToolResult {
    /// Plain text content, success.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text { text: s.into() }],
            is_error: false,
        }
    }

    /// Plain text content, marked as a tool-reported error.
    pub fn error(s: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text { text: s.into() }],
            is_error: true,
        }
    }

    /// JSON value rendered as compact text content. Convenience for
    /// returning structured data the client can parse.
    pub fn json<T: Serialize>(value: &T) -> Result<Self, RpcError> {
        let s = serde_json::to_string(value)
            .map_err(|e| RpcError::internal(format!("serialize tool result: {e}")))?;
        Ok(Self::text(s))
    }
}

/// One content block inside a [`ToolResult`] or prompt message.
/// Currently text-only; image/embedded-resource blocks come later.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Content {
    Text { text: String },
}

// ─── Resources ────────────────────────────────────────────────────────────

/// Resource descriptor advertised in `resources/list`. Concrete URI;
/// the resource is a single named thing (vs. a template that produces
/// many URIs).
#[derive(Debug, Clone, Serialize)]
pub struct Resource {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Builder for a static [`Resource`]. URI is required.
pub fn resource(uri: impl Into<String>, name: impl Into<String>) -> Resource {
    Resource {
        uri: uri.into(),
        name: name.into(),
        description: None,
        mime_type: None,
    }
}

impl Resource {
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }
    pub fn mime_type(mut self, m: impl Into<String>) -> Self {
        self.mime_type = Some(m.into());
        self
    }
}

/// Template resource descriptor advertised in `resources/templates/list`.
/// `uri_template` uses `{var}` placeholders; clients substitute and
/// then call `resources/read` with the concrete URI.
#[derive(Debug, Clone, Serialize)]
pub struct ResourceTemplate {
    #[serde(rename = "uriTemplate")]
    pub uri_template: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

pub fn resource_template(
    uri_template: impl Into<String>,
    name: impl Into<String>,
) -> ResourceTemplate {
    ResourceTemplate {
        uri_template: uri_template.into(),
        name: name.into(),
        description: None,
        mime_type: None,
    }
}

impl ResourceTemplate {
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }
    pub fn mime_type(mut self, m: impl Into<String>) -> Self {
        self.mime_type = Some(m.into());
        self
    }
}

/// One content block returned by a resource handler. Either text or
/// a base64-encoded blob — never both. (Spec: exactly one of `text`
/// or `blob` per content item.)
#[derive(Debug, Clone, Serialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

impl ResourceContent {
    pub fn text(uri: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            mime_type: None,
            text: Some(text.into()),
            blob: None,
        }
    }
    pub fn blob_base64(uri: impl Into<String>, blob: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            mime_type: None,
            text: None,
            blob: Some(blob.into()),
        }
    }
    pub fn mime_type(mut self, m: impl Into<String>) -> Self {
        self.mime_type = Some(m.into());
        self
    }
}

// ─── Prompts ──────────────────────────────────────────────────────────────

/// Prompt descriptor advertised in `prompts/list`.
#[derive(Debug, Clone, Serialize)]
pub struct Prompt {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<PromptArgument>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub required: bool,
}

pub fn prompt(name: impl Into<String>) -> Prompt {
    Prompt {
        name: name.into(),
        description: None,
        arguments: Vec::new(),
    }
}

impl Prompt {
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    /// Declare an argument the prompt accepts. `required: false` by
    /// default — chain `.required()` if needed. The SDK validates
    /// presence of required args before calling the handler.
    pub fn argument(mut self, arg: PromptArgument) -> Self {
        self.arguments.push(arg);
        self
    }
}

pub fn arg(name: impl Into<String>) -> PromptArgument {
    PromptArgument {
        name: name.into(),
        description: None,
        required: false,
    }
}

impl PromptArgument {
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
}

/// What a prompt handler returns. The SDK serializes this into the
/// `prompts/get` result (`{ description?, messages: [...] }`).
#[derive(Debug, Clone, Serialize)]
pub struct PromptResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub messages: Vec<PromptMessage>,
}

impl PromptResult {
    pub fn new() -> Self {
        Self { description: None, messages: Vec::new() }
    }
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }
    pub fn user_text(mut self, text: impl Into<String>) -> Self {
        self.messages.push(PromptMessage {
            role: Role::User,
            content: Content::Text { text: text.into() },
        });
        self
    }
    pub fn assistant_text(mut self, text: impl Into<String>) -> Self {
        self.messages.push(PromptMessage {
            role: Role::Assistant,
            content: Content::Text { text: text.into() },
        });
        self
    }
}

impl Default for PromptResult {
    fn default() -> Self {
        Self::new()
    }
}

/// One message in a [`PromptResult`]. MCP defines `role: "user" | "assistant"`.
#[derive(Debug, Clone, Serialize)]
pub struct PromptMessage {
    pub role: Role,
    pub content: Content,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// JSON-RPC server speaking the MCP protocol.
///
/// Build per request (cheap: registration is `Vec::push`). Same shape
/// as the existing `rpc::Dispatcher` — the handler in your
/// `build_router()` constructs one and calls `.handle(req)`.
pub struct McpServer {
    name: String,
    version: String,
    tools: Vec<ToolEntry>,
    resources: Vec<ResourceEntry>,
    resource_templates: Vec<ResourceTemplateEntry>,
    prompts: Vec<PromptEntry>,
}

impl McpServer {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            tools: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            prompts: Vec::new(),
        }
    }

    /// Register a raw tool with a `Fn(Value) -> Result<ToolResult, RpcError>`
    /// handler. **Prefer [`tool_typed`](Self::tool_typed)** — it's the
    /// symmetric counterpart to [`crate::rpc::Dispatcher::method`] and
    /// auto-derives the advertised `inputSchema` from `P`'s
    /// `JsonSchema` impl.
    ///
    /// Reach for this raw form only when you genuinely need full
    /// `ToolResult` control — multi-content-block responses, marked
    /// errors, embedded resource references — that the typed form
    /// hides behind `ToolResult::json(&result)`.
    ///
    /// ```ignore
    /// // Escape hatch: hand-rolled multi-content-block response.
    /// .tool(tool("rich").input_schema(json::json!({ "type": "object" })),
    ///       |args: Value| -> Result<ToolResult, RpcError> {
    ///     Ok(ToolResult { content: vec![/* multi-block */], is_error: false })
    /// })
    /// ```
    pub fn tool<F>(mut self, descriptor: Tool, handler: F) -> Self
    where
        F: Fn(Value) -> Result<ToolResult, RpcError> + 'static,
    {
        self.tools.push(ToolEntry {
            descriptor,
            handler: Box::new(handler),
        });
        self
    }

    /// Register a typed tool — the symmetric counterpart to
    /// [`crate::rpc::Dispatcher::method`].
    ///
    /// Handler shape: `Fn(P) -> Result<R, ApiError>` where:
    /// - `P: Deserialize + JsonSchema` — typed parameters. The
    ///   `inputSchema` advertised to clients is derived from `P`'s
    ///   `JsonSchema` impl, so the deserializer and the protocol
    ///   surface can't drift. If the caller already populated
    ///   `descriptor.input_schema()` with a custom schema, that
    ///   override is preserved; otherwise the derived schema replaces
    ///   the `tool()` builder's default `{"type": "object"}`.
    /// - `R: Serialize` — typed result. Wrapped automatically in
    ///   `ToolResult::json(&r)` so the wire shape stays MCP-conforming.
    /// - Error type is [`crate::error::ApiError`] (not `RpcError`) so a
    ///   shared business-logic function returning `Result<_, ApiError>`
    ///   slots into both REST and MCP surfaces unchanged.
    ///
    /// ```ignore
    /// #[derive(Deserialize, JsonSchema)]
    /// struct CreateNoteArgs { title: String, body: String }
    ///
    /// #[derive(Serialize)]
    /// struct NoteOut { id: String, title: String }
    ///
    /// .tool_typed(
    ///     tool("create_note").description("Create a note for the agent."),
    ///     |args: CreateNoteArgs| -> Result<NoteOut, ApiError> {
    ///         // ... build columns, insert, ApiError-on-failure ...
    ///     },
    /// )
    /// ```
    pub fn tool_typed<P, R, F>(self, mut descriptor: Tool, handler: F) -> Self
    where
        P: for<'de> serde::Deserialize<'de> + schemars::JsonSchema + 'static,
        R: serde::Serialize + 'static,
        F: Fn(P) -> Result<R, crate::error::ApiError> + 'static,
    {
        // Override the builder's default schema with one derived from P.
        // Honor an explicit `.input_schema(custom)` override the caller
        // chained — only fill in when the descriptor still carries the
        // `tool()` default. This lets authors hand-tune the schema for
        // edge cases (richer descriptions, examples, vendor extensions)
        // while keeping the common case zero-effort.
        if descriptor.input_schema == default_tool_input_schema() {
            descriptor.input_schema = serde_json::to_value(
                schemars::schema_for!(P).schema,
            )
            .unwrap_or_else(|_| default_tool_input_schema());
        }

        // Erase the typed handler into the underlying `Fn(Value) ->
        // Result<ToolResult, RpcError>` shape `tool()` already accepts.
        // Failure modes:
        //   - decode `Value -> P`           → `RpcError::invalid_params`
        //   - handler returned `ApiError`   → preserved verbatim through
        //     the `From<ApiError> for RpcError` impl (status code
        //     survives into the JSON-RPC application-error band)
        //   - serialize `R -> Value`        → `RpcError::internal`
        self.tool(descriptor, move |raw: Value| {
            let typed: P = serde_json::from_value(raw)
                .map_err(|e| RpcError::invalid_params(format!("{e}")))?;
            let result = handler(typed).map_err(RpcError::from)?;
            ToolResult::json(&result)
        })
    }

    /// Register a static resource. Handler is `Fn(&str) -> Result<Vec<ResourceContent>, RpcError>`
    /// — the str is the resolved URI (always equal to the descriptor's
    /// URI for static resources; templates fill in variables).
    pub fn resource<F>(mut self, descriptor: Resource, handler: F) -> Self
    where
        F: Fn(&str) -> Result<Vec<ResourceContent>, RpcError> + 'static,
    {
        self.resources.push(ResourceEntry {
            descriptor,
            handler: Box::new(handler),
        });
        self
    }

    /// Register a resource template. The descriptor's `uri_template`
    /// is parsed once; incoming `resources/read` URIs are matched
    /// against all registered templates in order, and the handler
    /// receives the matched URI verbatim. Use the helper
    /// [`extract_template_var`] if you need a specific variable.
    pub fn resource_template<F>(mut self, descriptor: ResourceTemplate, handler: F) -> Self
    where
        F: Fn(&str) -> Result<Vec<ResourceContent>, RpcError> + 'static,
    {
        let compiled = CompiledTemplate::parse(&descriptor.uri_template);
        self.resource_templates.push(ResourceTemplateEntry {
            descriptor,
            compiled,
            handler: Box::new(handler),
        });
        self
    }

    /// Register a prompt. Required arguments declared on the
    /// descriptor are validated by the SDK before the handler runs
    /// (missing → `invalid_params`).
    pub fn prompt<F>(mut self, descriptor: Prompt, handler: F) -> Self
    where
        F: Fn(std::collections::HashMap<String, String>) -> Result<PromptResult, RpcError>
            + 'static,
    {
        self.prompts.push(PromptEntry {
            descriptor,
            handler: Box::new(handler),
        });
        self
    }

    /// Dispatch one MCP request. Parses the JSON-RPC envelope, routes
    /// by method name, returns the appropriate JSON-RPC response.
    pub fn handle(&self, req: &crate::Request) -> response::HttpResponse {
        let Some(body) = &req.body else {
            return rpc::error_response(None, &RpcError::invalid_request("missing body"));
        };
        let envelope: rpc::Request = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => return rpc::error_response(None, &RpcError::parse_error(format!("{e}"))),
        };
        let id = envelope.id.as_ref();

        // Notifications (no id) get an empty 204 — JSON-RPC says no
        // response is sent. We pick 204 to keep the HTTP shape clean.
        let is_notification = envelope.id.is_none();

        match envelope.method.as_str() {
            "initialize" => rpc::success_response(id, &self.initialize_result()),
            "ping" => rpc::success_response(id, &serde_json::json!({})),
            "tools/list" => rpc::success_response(id, &self.tools_list_result()),
            "tools/call" => self.handle_tools_call(id, envelope.params),
            "resources/list" => rpc::success_response(id, &self.resources_list_result()),
            "resources/templates/list" => {
                rpc::success_response(id, &self.resources_templates_list_result())
            }
            "resources/read" => self.handle_resources_read(id, envelope.params),
            "prompts/list" => rpc::success_response(id, &self.prompts_list_result()),
            "prompts/get" => self.handle_prompts_get(id, envelope.params),
            "notifications/initialized" => {
                // Client-to-server notification confirming handshake.
                // Per JSON-RPC: no response body.
                if is_notification {
                    response::HttpResponse { status: 204, headers: vec![], body: None }
                } else {
                    rpc::success_response(id, &serde_json::json!({}))
                }
            }
            other => rpc::error_response(
                id,
                &RpcError::method_not_found(format!("method not found: {other}")),
            ),
        }
    }

    fn initialize_result(&self) -> Value {
        // Capabilities derived from what's registered. Each block is
        // an empty object when present — the spec allows extension
        // keys (subscribe, listChanged) but Phase 2 doesn't use them.
        let mut caps = serde_json::Map::new();
        if !self.tools.is_empty() {
            caps.insert("tools".into(), serde_json::json!({}));
        }
        if !self.resources.is_empty() || !self.resource_templates.is_empty() {
            caps.insert("resources".into(), serde_json::json!({}));
        }
        if !self.prompts.is_empty() {
            caps.insert("prompts".into(), serde_json::json!({}));
        }
        serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": Value::Object(caps),
            "serverInfo": {
                "name": self.name,
                "version": self.version,
            }
        })
    }

    fn tools_list_result(&self) -> Value {
        let descriptors: Vec<&Tool> = self.tools.iter().map(|t| &t.descriptor).collect();
        serde_json::json!({ "tools": descriptors })
    }

    fn resources_list_result(&self) -> Value {
        let descriptors: Vec<&Resource> =
            self.resources.iter().map(|r| &r.descriptor).collect();
        serde_json::json!({ "resources": descriptors })
    }

    fn resources_templates_list_result(&self) -> Value {
        let descriptors: Vec<&ResourceTemplate> = self
            .resource_templates
            .iter()
            .map(|r| &r.descriptor)
            .collect();
        serde_json::json!({ "resourceTemplates": descriptors })
    }

    fn prompts_list_result(&self) -> Value {
        let descriptors: Vec<&Prompt> = self.prompts.iter().map(|p| &p.descriptor).collect();
        serde_json::json!({ "prompts": descriptors })
    }

    fn handle_tools_call(&self, id: Option<&Value>, params: Value) -> response::HttpResponse {
        let start = Instant::now();
        // params shape: { "name": "...", "arguments": { ... } }
        let name = match params.get("name").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => {
                let resp = rpc::error_response(
                    id,
                    &RpcError::invalid_params("tools/call requires `name` (string)"),
                );
                return inject_mcp_telemetry(resp, "tool", "(missing)", start, "client_error");
            }
        };
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);

        let Some(entry) = self.tools.iter().find(|t| t.descriptor.name == name) else {
            let resp = rpc::error_response(
                id,
                &RpcError::method_not_found(format!("unknown tool: {name}")),
            );
            return inject_mcp_telemetry(resp, "tool", &name, start, "client_error");
        };

        match (entry.handler)(args) {
            Ok(result) => {
                let resp = rpc::success_response(id, &result);
                inject_mcp_telemetry(resp, "tool", &name, start, "success")
            }
            Err(e) => {
                let outcome = outcome_for_rpc_code(e.code);
                let resp = rpc::error_response(id, &e);
                inject_mcp_telemetry(resp, "tool", &name, start, outcome)
            }
        }
    }

    fn handle_resources_read(&self, id: Option<&Value>, params: Value) -> response::HttpResponse {
        let start = Instant::now();
        // params shape: { "uri": "..." }
        let uri = match params.get("uri").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => {
                let resp = rpc::error_response(
                    id,
                    &RpcError::invalid_params("resources/read requires `uri` (string)"),
                );
                return inject_mcp_telemetry(resp, "resource", "(missing)", start, "client_error");
            }
        };

        // Static resources first — exact URI match wins over templates.
        if let Some(entry) = self.resources.iter().find(|r| r.descriptor.uri == uri) {
            return match (entry.handler)(&uri) {
                Ok(contents) => {
                    let resp = rpc::success_response(
                        id,
                        &serde_json::json!({ "contents": contents }),
                    );
                    inject_mcp_telemetry(resp, "resource", &uri, start, "success")
                }
                Err(e) => {
                    let outcome = outcome_for_rpc_code(e.code);
                    let resp = rpc::error_response(id, &e);
                    inject_mcp_telemetry(resp, "resource", &uri, start, outcome)
                }
            };
        }

        // Then templates — first match wins. Registration order is
        // intentional: register more specific templates before more
        // generic catch-alls.
        for entry in &self.resource_templates {
            if entry.compiled.matches(&uri) {
                return match (entry.handler)(&uri) {
                    Ok(contents) => {
                        let resp = rpc::success_response(
                            id,
                            &serde_json::json!({ "contents": contents }),
                        );
                        inject_mcp_telemetry(resp, "resource_template", &uri, start, "success")
                    }
                    Err(e) => {
                        let outcome = outcome_for_rpc_code(e.code);
                        let resp = rpc::error_response(id, &e);
                        inject_mcp_telemetry(resp, "resource_template", &uri, start, outcome)
                    }
                };
            }
        }

        let resp = rpc::error_response(
            id,
            &RpcError::application(404, format!("resource not found: {uri}")),
        );
        inject_mcp_telemetry(resp, "resource", &uri, start, "client_error")
    }

    fn handle_prompts_get(&self, id: Option<&Value>, params: Value) -> response::HttpResponse {
        let start = Instant::now();
        // params shape: { "name": "...", "arguments": { "key": "value", ... } }
        let name = match params.get("name").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => {
                let resp = rpc::error_response(
                    id,
                    &RpcError::invalid_params("prompts/get requires `name` (string)"),
                );
                return inject_mcp_telemetry(resp, "prompt", "(missing)", start, "client_error");
            }
        };

        let Some(entry) = self.prompts.iter().find(|p| p.descriptor.name == name) else {
            let resp = rpc::error_response(
                id,
                &RpcError::method_not_found(format!("unknown prompt: {name}")),
            );
            return inject_mcp_telemetry(resp, "prompt", &name, start, "client_error");
        };

        // Coerce arguments to map<string, string>. MCP prompts pass
        // strings only; nested values would mean a richer prompt
        // template language, which v1 doesn't have.
        let mut args: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        if let Some(obj) = params.get("arguments").and_then(Value::as_object) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    args.insert(k.clone(), s.to_string());
                }
            }
        }

        // Validate declared required arguments before invoking the
        // handler — surfacing the standard JSON-RPC error keeps the
        // client experience consistent with tools/call.
        for declared in &entry.descriptor.arguments {
            if declared.required && !args.contains_key(&declared.name) {
                let resp = rpc::error_response(
                    id,
                    &RpcError::invalid_params(format!(
                        "missing required argument: {}",
                        declared.name
                    )),
                );
                return inject_mcp_telemetry(resp, "prompt", &name, start, "client_error");
            }
        }

        match (entry.handler)(args) {
            Ok(result) => {
                let resp = rpc::success_response(id, &result);
                inject_mcp_telemetry(resp, "prompt", &name, start, "success")
            }
            Err(e) => {
                let outcome = outcome_for_rpc_code(e.code);
                let resp = rpc::error_response(id, &e);
                inject_mcp_telemetry(resp, "prompt", &name, start, outcome)
            }
        }
    }
}

// ─── URI template matching ────────────────────────────────────────────────
//
// We don't pull in a full RFC 6570 implementation. Templates use the
// same `{var}` placeholder shape as the SDK's path router. Variables
// match greedily up to the next literal segment or end-of-string.
// Multi-variable templates work as long as adjacent variables are
// separated by at least one literal character.

struct CompiledTemplate {
    parts: Vec<TemplatePart>,
}

enum TemplatePart {
    Literal(String),
    Var,
}

impl CompiledTemplate {
    fn parse(template: &str) -> Self {
        let mut parts = Vec::new();
        let mut i = 0;
        let bytes = template.as_bytes();
        let mut buf = String::new();
        while i < bytes.len() {
            if bytes[i] == b'{' {
                if !buf.is_empty() {
                    parts.push(TemplatePart::Literal(std::mem::take(&mut buf)));
                }
                while i < bytes.len() && bytes[i] != b'}' {
                    i += 1;
                }
                // Skip the '}' if found; if the template is malformed
                // (unclosed brace) we stop here — the resulting
                // template will simply never match anything.
                if i < bytes.len() {
                    i += 1;
                }
                parts.push(TemplatePart::Var);
            } else {
                buf.push(bytes[i] as char);
                i += 1;
            }
        }
        if !buf.is_empty() {
            parts.push(TemplatePart::Literal(buf));
        }
        Self { parts }
    }

    fn matches(&self, uri: &str) -> bool {
        let mut rest = uri;
        let mut iter = self.parts.iter().peekable();
        while let Some(part) = iter.next() {
            match part {
                TemplatePart::Literal(lit) => {
                    if !rest.starts_with(lit.as_str()) {
                        return false;
                    }
                    rest = &rest[lit.len()..];
                }
                TemplatePart::Var => {
                    // Variable consumes up to the next literal (or to
                    // end-of-string). At least one byte is required —
                    // empty matches don't make sense for resource ids.
                    let next_lit = match iter.peek() {
                        Some(TemplatePart::Literal(l)) => Some(l.as_str()),
                        _ => None,
                    };
                    match next_lit {
                        Some(lit) => {
                            let Some(end) = rest.find(lit) else {
                                return false;
                            };
                            if end == 0 {
                                return false;
                            }
                            rest = &rest[end..];
                        }
                        None => {
                            // Variable goes to end-of-string.
                            if rest.is_empty() {
                                return false;
                            }
                            rest = "";
                        }
                    }
                }
            }
        }
        rest.is_empty()
    }
}

/// Pull a single named variable out of a URI matched against a template.
/// Returns `None` if either the URI doesn't match the template or the
/// variable doesn't exist in the template. Useful inside resource
/// handlers to recover the var the client supplied.
///
/// ```ignore
/// let id = extract_template_var("notes://{id}", uri, "id")?;
/// ```
pub fn extract_template_var(template: &str, uri: &str, var_name: &str) -> Option<String> {
    // Walk the template tracking variable positions; when we hit the
    // named one, capture its slice from the uri.
    let bytes = template.as_bytes();
    let uri_bytes = uri.as_bytes();
    let mut t_i = 0;
    let mut u_i = 0;
    while t_i < bytes.len() {
        if bytes[t_i] == b'{' {
            // Read variable name.
            let name_start = t_i + 1;
            let mut name_end = name_start;
            while name_end < bytes.len() && bytes[name_end] != b'}' {
                name_end += 1;
            }
            let cur_name = std::str::from_utf8(&bytes[name_start..name_end]).ok()?;
            t_i = name_end.saturating_add(1);
            // Find the next literal.
            let next_lit_start = t_i;
            let mut next_lit_end = next_lit_start;
            while next_lit_end < bytes.len() && bytes[next_lit_end] != b'{' {
                next_lit_end += 1;
            }
            let next_lit = std::str::from_utf8(&bytes[next_lit_start..next_lit_end]).ok()?;
            // Capture from uri up to next_lit (or end).
            let capture_end = if next_lit.is_empty() {
                uri_bytes.len()
            } else {
                let rest = std::str::from_utf8(&uri_bytes[u_i..]).ok()?;
                u_i + rest.find(next_lit)?
            };
            let captured = std::str::from_utf8(&uri_bytes[u_i..capture_end]).ok()?;
            if cur_name == var_name {
                return Some(captured.to_string());
            }
            // Consume the trailing literal in BOTH the template and
            // the URI so the next variable starts from the right place.
            u_i = capture_end + next_lit.len();
            t_i = next_lit_end;
        } else {
            // Literal byte must match.
            if u_i >= uri_bytes.len() || uri_bytes[u_i] != bytes[t_i] {
                return None;
            }
            t_i += 1;
            u_i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with_body(body: Value) -> crate::Request {
        crate::Request {
            method: "POST".into(),
            path: "/mcp".into(),
            headers: vec![],
            body: Some(body.to_string().into_bytes()),
            path_params: vec![],
            query_params: vec![],
        }
    }

    fn parse_body(resp: &response::HttpResponse) -> Value {
        serde_json::from_slice(resp.body.as_ref().expect("body")).unwrap()
    }

    #[test]
    fn initialize_returns_protocol_version_and_server_info() {
        let server = McpServer::new("test", "0.1.0");
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        })));
        let body = parse_body(&resp);
        assert_eq!(body["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(body["result"]["serverInfo"]["name"], "test");
        assert_eq!(body["result"]["serverInfo"]["version"], "0.1.0");
    }

    #[test]
    fn initialize_advertises_tools_capability_only_when_registered() {
        let bare = McpServer::new("bare", "0.1.0");
        let bare_resp = bare.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}
        })));
        let bare_caps = &parse_body(&bare_resp)["result"]["capabilities"];
        assert!(bare_caps.get("tools").is_none(), "no tools registered → no tools capability");

        let with_tool = McpServer::new("withtool", "0.1.0").tool(
            tool("noop"),
            |_| Ok(ToolResult::text("ok")),
        );
        let resp = with_tool.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}
        })));
        assert!(parse_body(&resp)["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_returns_registered_tools() {
        let server = McpServer::new("test", "0.1.0")
            .tool(
                tool("greet").description("Say hello").input_schema(serde_json::json!({
                    "type": "object",
                    "properties": { "name": { "type": "string" } }
                })),
                |_| Ok(ToolResult::text("hi")),
            )
            .tool(tool("ping_back"), |_| Ok(ToolResult::text("pong")));

        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list"
        })));
        let body = parse_body(&resp);
        let tools = body["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "greet");
        assert_eq!(tools[0]["description"], "Say hello");
        assert_eq!(tools[1]["name"], "ping_back");
        // Tools without a description omit the field entirely.
        assert!(tools[1].get("description").is_none());
    }

    #[test]
    fn tools_call_dispatches_to_handler() {
        let server = McpServer::new("test", "0.1.0").tool(
            tool("echo").input_schema(serde_json::json!({
                "type": "object",
                "required": ["text"],
                "properties": { "text": { "type": "string" } }
            })),
            |args| {
                let text = args.get("text").and_then(Value::as_str).unwrap_or("");
                Ok(ToolResult::text(format!("echo: {text}")))
            },
        );

        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "echo",
                "arguments": { "text": "hello" }
            }
        })));
        let body = parse_body(&resp);
        assert_eq!(body["result"]["content"][0]["type"], "text");
        assert_eq!(body["result"]["content"][0]["text"], "echo: hello");
        // is_error is false-by-default and skipped from the wire by serde.
        assert!(body["result"].get("isError").is_none());
    }

    #[test]
    fn tools_call_unknown_tool_is_method_not_found() {
        let server = McpServer::new("test", "0.1.0");
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "nope" }
        })));
        let body = parse_body(&resp);
        assert_eq!(body["error"]["code"], -32601);
    }

    #[test]
    fn tools_call_missing_name_is_invalid_params() {
        let server = McpServer::new("test", "0.1.0");
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {}
        })));
        let body = parse_body(&resp);
        assert_eq!(body["error"]["code"], -32602);
    }

    #[test]
    fn tool_reported_error_surfaces_as_ok_with_is_error_flag() {
        // A failing tool returns Ok(ToolResult::error(...)). The wire
        // shape is a JSON-RPC SUCCESS with `result.isError = true` —
        // distinct from JSON-RPC errors (which signal protocol problems).
        let server = McpServer::new("test", "0.1.0").tool(
            tool("must_fail"),
            |_| Ok(ToolResult::error("intentional")),
        );
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": { "name": "must_fail" }
        })));
        let body = parse_body(&resp);
        assert!(body.get("error").is_none(), "should not be a JSON-RPC error");
        assert_eq!(body["result"]["isError"], true);
        assert_eq!(body["result"]["content"][0]["text"], "intentional");
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let server = McpServer::new("test", "0.1.0");
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "completions/complete"
        })));
        let body = parse_body(&resp);
        assert_eq!(body["error"]["code"], -32601);
    }

    #[test]
    fn ping_returns_empty_object() {
        let server = McpServer::new("test", "0.1.0");
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 8, "method": "ping"
        })));
        let body = parse_body(&resp);
        assert!(body["result"].is_object());
        assert!(body.get("error").is_none());
    }

    #[test]
    fn initialized_notification_returns_204_with_no_body() {
        let server = McpServer::new("test", "0.1.0");
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
            // no `id` → notification
        })));
        assert_eq!(resp.status, 204);
        assert!(resp.body.is_none());
    }

    #[test]
    fn missing_body_returns_invalid_request() {
        let server = McpServer::new("test", "0.1.0");
        let req = crate::Request {
            method: "POST".into(),
            path: "/mcp".into(),
            headers: vec![],
            body: None,
            path_params: vec![],
            query_params: vec![],
        };
        let resp = server.handle(&req);
        let body = parse_body(&resp);
        assert_eq!(body["error"]["code"], -32600);
    }

    #[test]
    fn malformed_body_returns_parse_error() {
        let server = McpServer::new("test", "0.1.0");
        let req = crate::Request {
            method: "POST".into(),
            path: "/mcp".into(),
            headers: vec![],
            body: Some(b"{not json".to_vec()),
            path_params: vec![],
            query_params: vec![],
        };
        let resp = server.handle(&req);
        let body = parse_body(&resp);
        assert_eq!(body["error"]["code"], -32700);
    }

    // ─── tool_typed (B1 + B2) ──────────────────────────────────────────────

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct GreetArgs {
        name: String,
    }

    #[derive(serde::Serialize)]
    struct GreetResult {
        message: String,
    }

    #[test]
    fn tool_typed_decodes_params_and_serializes_result() {
        let server = McpServer::new("typed", "0.1.0").tool_typed(
            tool("greet"),
            |args: GreetArgs| -> Result<GreetResult, crate::error::ApiError> {
                Ok(GreetResult { message: format!("hi {}", args.name) })
            },
        );
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "greet", "arguments": { "name": "alice" } }
        })));
        let body = parse_body(&resp);
        // Tool results carry text content blocks; the typed result is
        // JSON-stringified inside the first content block.
        let inner_text = body["result"]["content"][0]["text"].as_str().unwrap();
        let inner: Value = serde_json::from_str(inner_text).unwrap();
        assert_eq!(inner["message"], "hi alice");
    }

    #[test]
    fn tool_typed_invalid_params_yields_invalid_params_error() {
        let server = McpServer::new("typed", "0.1.0").tool_typed(
            tool("greet"),
            |args: GreetArgs| -> Result<GreetResult, crate::error::ApiError> {
                Ok(GreetResult { message: format!("hi {}", args.name) })
            },
        );
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "greet", "arguments": { "name": 42 } }
        })));
        let body = parse_body(&resp);
        assert_eq!(body["error"]["code"], -32602, "wrong-typed args → invalid_params");
    }

    #[test]
    fn tool_typed_handler_api_error_preserves_status_code() {
        let server = McpServer::new("typed", "0.1.0").tool_typed(
            tool("greet"),
            |_args: GreetArgs| -> Result<GreetResult, crate::error::ApiError> {
                Err(crate::error::ApiError::not_found())
            },
        );
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "greet", "arguments": { "name": "ghost" } }
        })));
        let body = parse_body(&resp);
        // ApiError status round-trips through `From<ApiError> for RpcError`
        // as the application-error code (the HTTP status as an i64).
        assert_eq!(body["error"]["code"], 404);
    }

    #[test]
    fn tool_typed_derives_input_schema_from_jsonschema() {
        let server = McpServer::new("typed", "0.1.0").tool_typed(
            tool("greet").description("Greet someone"),
            |args: GreetArgs| -> Result<GreetResult, crate::error::ApiError> {
                Ok(GreetResult { message: format!("hi {}", args.name) })
            },
        );
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}
        })));
        let body = parse_body(&resp);
        let schema = &body["result"]["tools"][0]["inputSchema"];
        // Derived from GreetArgs — must mention `name` as a property.
        let props = &schema["properties"];
        assert!(props.get("name").is_some(),
            "schemars derived schema should include the `name` field; got: {schema}");
    }

    #[test]
    fn tool_typed_respects_explicit_input_schema_override() {
        let custom = serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string", "minLength": 1 } },
            "required": ["name"],
            "description": "hand-tuned"
        });
        let server = McpServer::new("typed", "0.1.0").tool_typed(
            tool("greet").input_schema(custom.clone()),
            |args: GreetArgs| -> Result<GreetResult, crate::error::ApiError> {
                Ok(GreetResult { message: format!("hi {}", args.name) })
            },
        );
        let resp = server.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}
        })));
        let body = parse_body(&resp);
        let schema = &body["result"]["tools"][0]["inputSchema"];
        // Explicit override wins — the derived schema must not stomp it.
        assert_eq!(schema["description"], "hand-tuned");
    }

    // ─── Phase 2 — resources + prompts ──────────────────────────────────

    #[test]
    fn initialize_advertises_resources_when_static_or_template_registered() {
        // Static resource alone → resources capability present.
        let s = McpServer::new("t", "0.1.0").resource(resource("config://app", "config"), |_| {
            Ok(vec![ResourceContent::text("config://app", "{}")])
        });
        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize"
        }))));
        assert!(body["result"]["capabilities"]["resources"].is_object());

        // Template alone is also enough.
        let s2 = McpServer::new("t", "0.1.0").resource_template(
            resource_template("note://{id}", "note"),
            |_uri| Ok(vec![]),
        );
        let body2 = parse_body(&s2.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize"
        }))));
        assert!(body2["result"]["capabilities"]["resources"].is_object());
    }

    #[test]
    fn initialize_advertises_prompts_when_registered() {
        let s = McpServer::new("t", "0.1.0").prompt(
            prompt("greet").description("hi"),
            |_args| Ok(PromptResult::new().user_text("hello")),
        );
        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize"
        }))));
        assert!(body["result"]["capabilities"]["prompts"].is_object());
    }

    #[test]
    fn resources_list_returns_static_resources_only() {
        let s = McpServer::new("t", "0.1.0")
            .resource(
                resource("file://config", "Config")
                    .description("App config")
                    .mime_type("application/json"),
                |_| Ok(vec![ResourceContent::text("file://config", "{}")]),
            )
            .resource_template(resource_template("note://{id}", "note"), |_| Ok(vec![]));

        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "resources/list"
        }))));
        let resources = body["result"]["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 1, "templates must not appear in resources/list");
        assert_eq!(resources[0]["uri"], "file://config");
        assert_eq!(resources[0]["mimeType"], "application/json");
    }

    #[test]
    fn resources_templates_list_returns_templates_only() {
        let s = McpServer::new("t", "0.1.0")
            .resource(resource("file://config", "Config"), |_| Ok(vec![]))
            .resource_template(
                resource_template("note://{id}", "note")
                    .description("a note")
                    .mime_type("text/plain"),
                |_| Ok(vec![]),
            );

        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "resources/templates/list"
        }))));
        let templates = body["result"]["resourceTemplates"].as_array().unwrap();
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0]["uriTemplate"], "note://{id}");
    }

    #[test]
    fn resources_read_static_uri_calls_handler() {
        let s = McpServer::new("t", "0.1.0").resource(
            resource("file://hello", "hello"),
            |uri| Ok(vec![ResourceContent::text(uri, "world").mime_type("text/plain")]),
        );
        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "resources/read",
            "params": { "uri": "file://hello" }
        }))));
        let contents = body["result"]["contents"].as_array().unwrap();
        assert_eq!(contents[0]["uri"], "file://hello");
        assert_eq!(contents[0]["text"], "world");
        assert_eq!(contents[0]["mimeType"], "text/plain");
    }

    #[test]
    fn resources_read_template_matches_and_routes() {
        let s = McpServer::new("t", "0.1.0").resource_template(
            resource_template("note://{id}", "note"),
            |uri| {
                let id = extract_template_var("note://{id}", uri, "id").unwrap_or_default();
                Ok(vec![ResourceContent::text(uri, format!("note id={id}"))])
            },
        );
        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "resources/read",
            "params": { "uri": "note://abc123" }
        }))));
        let contents = body["result"]["contents"].as_array().unwrap();
        assert_eq!(contents[0]["text"], "note id=abc123");
    }

    #[test]
    fn resources_read_unknown_uri_is_404() {
        let s = McpServer::new("t", "0.1.0");
        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "resources/read",
            "params": { "uri": "nothing://here" }
        }))));
        assert_eq!(body["error"]["code"], 404);
    }

    #[test]
    fn resources_read_static_takes_precedence_over_template() {
        // Both register URIs that match `note://specific`. The static
        // entry must win (registration order shouldn't override
        // exact-URI semantics).
        let s = McpServer::new("t", "0.1.0")
            .resource_template(resource_template("note://{id}", "note"), |uri| {
                Ok(vec![ResourceContent::text(uri, "from template")])
            })
            .resource(resource("note://specific", "specific"), |uri| {
                Ok(vec![ResourceContent::text(uri, "from static")])
            });
        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "resources/read",
            "params": { "uri": "note://specific" }
        }))));
        assert_eq!(body["result"]["contents"][0]["text"], "from static");
    }

    #[test]
    fn prompts_list_returns_descriptors_with_arguments() {
        let s = McpServer::new("t", "0.1.0").prompt(
            prompt("summarize")
                .description("Summarize a body of text")
                .argument(arg("text").description("the text").required())
                .argument(arg("style")),
            |_| Ok(PromptResult::new()),
        );
        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "prompts/list"
        }))));
        let prompts = body["result"]["prompts"].as_array().unwrap();
        assert_eq!(prompts[0]["name"], "summarize");
        assert_eq!(prompts[0]["arguments"][0]["name"], "text");
        assert_eq!(prompts[0]["arguments"][0]["required"], true);
        // `required: false` is omitted by serde — it's the default.
        assert!(prompts[0]["arguments"][1].get("required").is_none());
    }

    #[test]
    fn prompts_get_returns_rendered_messages() {
        let s = McpServer::new("t", "0.1.0").prompt(
            prompt("greet").argument(arg("name").required()),
            |args| {
                let name = args.get("name").cloned().unwrap_or_default();
                Ok(PromptResult::new()
                    .description("a greeting")
                    .user_text(format!("hello {name}!")))
            },
        );
        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "prompts/get",
            "params": { "name": "greet", "arguments": { "name": "alice" } }
        }))));
        assert_eq!(body["result"]["description"], "a greeting");
        assert_eq!(body["result"]["messages"][0]["role"], "user");
        assert_eq!(body["result"]["messages"][0]["content"]["text"], "hello alice!");
    }

    #[test]
    fn prompts_get_missing_required_argument_is_invalid_params() {
        let s = McpServer::new("t", "0.1.0").prompt(
            prompt("greet").argument(arg("name").required()),
            |_| Ok(PromptResult::new()),
        );
        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "prompts/get",
            "params": { "name": "greet", "arguments": {} }
        }))));
        assert_eq!(body["error"]["code"], -32602);
    }

    #[test]
    fn prompts_get_unknown_prompt_is_method_not_found() {
        let s = McpServer::new("t", "0.1.0");
        let body = parse_body(&s.handle(&req_with_body(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "prompts/get",
            "params": { "name": "missing" }
        }))));
        assert_eq!(body["error"]["code"], -32601);
    }

    // ─── URI template matcher ───────────────────────────────────────────

    #[test]
    fn template_matcher_handles_single_var() {
        let t = CompiledTemplate::parse("note://{id}");
        assert!(t.matches("note://abc"));
        assert!(t.matches("note://x")); // single char is fine
        assert!(!t.matches("note://")); // empty var rejected
        assert!(!t.matches("notez://abc")); // wrong literal
    }

    #[test]
    fn template_matcher_handles_multiple_vars() {
        let t = CompiledTemplate::parse("users/{user}/notes/{id}");
        assert!(t.matches("users/alice/notes/n1"));
        assert!(!t.matches("users/alice/notes/")); // trailing var empty
        assert!(!t.matches("users//notes/n1")); // first var empty
        assert!(!t.matches("users/alice/notes")); // missing trailing var
    }

    #[test]
    fn extract_template_var_pulls_named_value() {
        let id = extract_template_var("note://{id}", "note://abc123", "id");
        assert_eq!(id.as_deref(), Some("abc123"));

        let user = extract_template_var(
            "users/{user}/notes/{id}",
            "users/alice/notes/n42",
            "user",
        );
        assert_eq!(user.as_deref(), Some("alice"));
        let id = extract_template_var(
            "users/{user}/notes/{id}",
            "users/alice/notes/n42",
            "id",
        );
        assert_eq!(id.as_deref(), Some("n42"));
    }

    #[test]
    fn extract_template_var_returns_none_for_unknown_name() {
        let v = extract_template_var("note://{id}", "note://abc", "missing");
        assert!(v.is_none());
    }
}
