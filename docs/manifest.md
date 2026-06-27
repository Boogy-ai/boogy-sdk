# Boogy Manifest Reference

Every deployed Boogy service has a `boogy.toml` manifest next to its crate. The manifest tells the host how to route requests to your service, what capabilities it needs, and how to gate access. You pass the path to this file when deploying:

```bash
boogy deploy path/to/my-service/boogy.toml
```

The host parses and validates the manifest on every deploy. Unknown fields in `[outbound]` are rejected; other sections accept extra fields silently (subject to change — stick to documented fields).

---

## Worked example

A realistic notes service with persistent storage, per-user auth, rate limiting, and a scheduled cleanup job:

```toml
[service]
id = "notes"
name = "Notes API"
version = "0.2.0"
wasm = "target/wasm32-wasip2/release/notes_api.wasm"
description = "Per-user notes with tagging and full-text search."
keywords = ["notes", "personal", "storage"]
category = "productivity"
owner = "alice"   # optional — the platform sets this to your handle at deploy; normally omit it

[routing]
path = "/api/notes"
methods = ["GET", "POST", "PUT", "DELETE"]

[capabilities]
store = true
auth = true
clock = true
entropy = true
logging = true
background_jobs = true

[limits]
memory_mb = 64
timeout_ms = 10000
cpu_deadline_ms = 15000
storage_mb = 512

[ingress]
mode = "authenticated"

[ingress.rate_limit]
rpm = 600
burst = 60

[background_jobs.handlers.cleanup_old_notes]
deadline_ms = 30000
max_attempts = 3
backoff_ms = 5000
max_concurrent_per_tenant = 2
schedule = "0 0 3 * * *"   # 03:00 UTC daily  (sec min hour day month dow)
```

---

## `[service]`

Top-level service identity. All fields in this section are **required** unless marked optional.

| Field | Type | Required / Default | Meaning |
|---|---|---|---|
| `id` | string | **required** | Stable identifier for this service. ASCII alphanumeric, `-`, `_`; max 64 chars; no leading `-`; no dots or slashes. Used in URLs and on-disk paths. |
| `name` | string | **required** | Human-readable display name. |
| `version` | string | **required** | SemVer string (e.g. `"0.2.0"`). Stored with the deployment; not used for routing. |
| `wasm` | string | optional, `""` | Path to the compiled `.wasm` file, relative to the manifest (typically `target/wasm32-wasip2/release/<crate_name>.wasm`). **Omit it for a frontend-only (`Frontend` shape) deployment** — it runs no wasm. A `Service`/`FullStack` deploy with no wasm *and* no `[frontend]` is rejected ("nothing to deploy"). |
| `description` | string | optional, `null` | One-paragraph description. Max 2000 chars. |
| `keywords` | string array | optional, `[]` | Searchable tags. At most 40 entries; each ≤ 64 chars. |
| `category` | string | optional, `null` | Category tag for grouping. Max 64 chars. |
| `owner` | string | optional, `""` | The owning user's handle, as a **bare key** (`owner = "alice"`), NOT a `[service.owner]` table. Normally **omit it** — you are authenticated when you deploy, so the platform sets the owner to your handle at publish/provision and overwrites any value here. Keep it only for local-dev/tests that provision under a fixed owner without the auth flow. Same character rules as `service.id`; not a platform-reserved name (`v1`, `healthz`, `_admin`, `_agents`, `_sys`). |

> The `owner` + `id` pair is the unique key for a deployment: deploying the same pair replaces the running service. Since the platform fills `owner` from your authenticated handle, you normally only set `id`.

---

## `[routing]`

Both fields are **required**.

| Field | Type | Required / Default | Meaning |
|---|---|---|---|
| `path` | string | **required** | URL path prefix your service owns, e.g. `"/api/notes"`. The host resolves the owner from the subdomain (`<handle>.boogy.app`) and internally synthesizes a `/{owner}/{path}` routing key; the `/{owner}` prefix is stripped before your handler sees the URL. Use `"/"` to own the full owner subtree. |
| `methods` | string array | **required** | HTTP methods to accept: `["GET", "POST"]`. Use `["*"]` to match any method. |

---

## `[capabilities]`

The `[capabilities]` section is **optional** — a missing or empty one grants nothing (deny-by-default). Each capability defaults to `false`; a service that doesn't declare a capability gets a host error if its wasm code attempts to use it. (A frontend-only deployment grants none and may omit the section entirely.)

| Field | Type | Default | Meaning |
|---|---|---|---|
| `store` | bool | `false` | Isolated, transactional storage scoped to this service. |
| `auth` | bool | `false` | Read the caller's identity (`auth::current_principal()`, ownership guards). |
| `clock` | bool | `false` | Read the current wall-clock time. |
| `entropy` | bool | `false` | Cryptographic random bytes. |
| `logging` | bool | `false` | Write to the platform log stream. |
| `peer` | bool | `false` | Call other deployed services via `peer::fetch` (in-process, no network hop). |
| `outbound_http` | bool | `false` | Make HTTPS calls to external URLs. Requires a `[outbound]` block with non-empty `allowed_hosts`. |
| `background_jobs` | bool | `false` | Enqueue and manage background jobs (`jobs::enqueue` / `jobs::cancel` / `jobs::status`). |
| `signing` | bool | `false` | Produce cryptographic signatures (ECDSA secp256k1 / P-256, Ed25519) with a private key the host holds and your code never sees — only a public key and the signature come back. |
| `websockets` | bool | `false` | Publish real-time messages to end-user clients over channels declared in `[[websockets.channels]]`. |

---

## `[limits]`

The `[limits]` section is **optional** — a missing or empty one takes the per-field defaults below.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `memory_mb` | u32 | `32` | Per-request Wasm linear memory cap in MiB. |
| `timeout_ms` | u64 | `5000` | Legacy wall-clock timeout in ms. |
| `cpu_deadline_ms` | u64 | `30000` | Per-request wall-clock budget `B_req` in ms. The scheduler uses this as the slot-holding ceiling. Epoch deadline traps a CPU-bound guest; an outer timeout returns HTTP 504. Range: 1–600000. |
| `storage_mb` | u32 or null | `null` (platform default) | Soft storage quota in MiB. `null` = use the platform default; `0` = unlimited (trusted opt-out). |

---

## `[frontend]`

Optional. Declares a web frontend the **platform serves for you** — no
bundler, no Node, no JS toolchain. You ship TypeScript/JS/HTML/CSS source;
the control plane transpiles it at deploy and serves the assets from object
storage, decoupled from your wasm. Presence + whether you publish a wasm
derives the **deployment shape**:

- `[frontend]` **+ wasm** → **full-stack** (UI + API, one origin).
- `[frontend]`, **no wasm** → **frontend-only** (a static site / SPA).
- no `[frontend]` → a plain **service** (wasm only).

| Field | Type | Default | Meaning |
|---|---|---|---|
| `root` | string | **required** | Source dir holding the frontend (e.g. `index.html` + `.ts`/`.css`/assets). Safe relative path — no `..`, no leading `/`. |
| `api_prefix` | string | — | Full-stack only: requests under this prefix go to the wasm backend; everything else is served as a static asset. Must start with `/`. Omit for a frontend-only deployment. |
| `index` | string | `"index.html"` | SPA entry document, served for extensionless / fallback routes. |
| `build` | string | `"ts"` | `"ts"` (platform transpiles TypeScript) or `"none"` (assets are already built). |
| `private` | bool | `false` | `true` gates asset serving behind the service ingress (a private app). Default public. |
| `allow_cdn` | bool | `false` | When a bare import isn't vendored under `/vendor/`, resolve it to an `esm.sh` CDN URL in the generated import map instead of failing the build. |
| `minify` | bool | on when `build = "ts"` | Minify (compact) the transpiled `.ts` → `.js` output at deploy. Defaults on whenever the bundle is transpiled; set `minify = false` to ship readable JS for debugging. Compaction only (whitespace/optional tokens); passthrough `.js` is served verbatim. |
| `csp` | string | — | Opt-in `Content-Security-Policy`, emitted verbatim on served responses. Unset = no CSP header. A safe baseline (`X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options`) is always on. |
| `frame_options` | string | `"same_origin"` | `same_origin` (→ `SAMEORIGIN`), `deny` (→ `DENY`), or `none` (omit the header, for apps meant to be embedded). |

```toml
[frontend]
root = "web"
api_prefix = "/api"   # full-stack; omit for a frontend-only site
build = "ts"
private = false
```

A frontend-only deployment needs **no `[capabilities]`, `[ingress]`, or data
model** — it runs no wasm. See the `boogy-serving-frontends` skill.

---

## `[ingress]`

Controls who can call your service. The entire section is optional; omitting it is equivalent to `mode = "public"`.

### Mode guide

| Mode | Who can call | When to use |
|---|---|---|
| `"public"` | Anyone, including unauthenticated | Static content, public APIs, redirect handlers |
| `"authenticated"` | Any agent or workload with a valid token | Most user-facing APIs |
| `"allowlist"` | Agents listed in `allowed_agents` | Invite-only APIs, beta access |
| `"internal"` | Workloads listed in `allowed_origins` | Service-to-service only, no human callers |
| `"mixed"` | Agents in `allowed_agents` OR workloads in `allowed_origins` | APIs serving both a public UI and an internal mesh |

### Fields

| Field | Type | Default | Meaning |
|---|---|---|---|
| `mode` | string | `"public"` | One of the five modes above. |
| `allowed_agents` | string array | `[]` | For `allowlist` / `mixed`: agent matchers. Each entry is `*` (any agent), `@handle` (by handle, case-insensitive), or `agent_<uuid>` (exact ID). **Required non-empty when `mode = "allowlist"`.** |
| `allowed_origins` | string array | `[]` | For `internal` / `mixed`: workload URI matchers. Each entry is `*` / `boogy://*` (any workload), `boogy://<owner>/*` (any service owned by `<owner>`), or `boogy://<owner>/services/<name>` (exact service). **Required non-empty when `mode = "internal"`.** |

### `[ingress.rate_limit]`

Token-bucket rate limiter. Applied after auth checks pass — denied requests do not consume budget.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `rpm` | u32 | — | Requests per minute (refill rate). |
| `burst` | u32 or null | `rpm / 60` | Burst capacity. Omit to use the default of one second's worth. |

### `[ingress.delegation]`

Opt-in on-behalf-of (OBO) policy. Absent = delegated (actor-bearing) tokens are **rejected**. Present = delegated calls are accepted when they satisfy the rules below.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `allow_actor` | string array | `[]` | Workload URIs permitted to deliver requests on behalf of agents. Same matcher syntax as `allowed_origins`. Empty = delegation disabled even if the section is present. |
| `max_delegated_scopes` | string array | `[]` | Optional scope cap. When non-empty, every scope on the inbound token must match at least one entry. Matchers: `*` (any), `resource:action` (exact), `resource:*` (any action), `*:action` (any resource). Empty = no scope cap. |
| `require_principal_in_allowed_agents` | bool | `false` | When `true`, the principal (the agent being acted for) must also appear in `allowed_agents`. |

```toml
[ingress]
mode = "authenticated"

[ingress.delegation]
allow_actor = ["boogy://alice/services/gateway"]
max_delegated_scopes = ["notes:*"]
```

### `[ingress.cors]`

Opt-in, host-enforced cross-origin allowlist. Absent = no CORS headers emitted (default-deny; browsers block cross-origin reads). Only relevant when a **different** origin calls your API — a same-origin full-stack page needs none. The host answers `OPTIONS` preflights at the edge and reflects the allowed origin on actual responses. **CORS is not authorization** — an allowed origin still passes the normal ingress (token) check.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `allowed_origins` | string array | `[]` | Exact origins (`https://app.example.com`), or `["*"]` to allow any — permitted only when `allow_credentials = false`. |
| `allowed_methods` | string array | `[]` | Methods echoed on preflight. Empty = a safe default set. |
| `allowed_headers` | string array | `[]` | Request headers echoed on preflight. |
| `allow_credentials` | bool | `false` | Allow cookie/`Authorization` requests. `true` forbids `allowed_origins = ["*"]` (rejected at deploy). |
| `max_age` | u64 | — | Preflight cache lifetime in seconds. |

```toml
[ingress.cors]
allowed_origins = ["https://app.example.com"]
allowed_methods = ["GET", "POST"]
allow_credentials = false
```

---

## `[outbound]`

Required (with non-empty `allowed_hosts`) when `capabilities.outbound_http = true`. Ignored when the capability is off.

`[outbound]` uses `deny_unknown_fields` — any unrecognised key is a parse error.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `allowed_hosts` | string array | `[]` | Glob patterns for permitted HTTPS destinations. Supports `*` wildcards (e.g. `"*.openai.com"`). Must be non-empty when `outbound_http` is granted. |
| `max_timeout_ms` | u32 | `30000` | Hard ceiling on the per-call timeout wasm can request. |
| `default_timeout_ms` | u32 | `10000` | Per-call timeout when wasm doesn't specify one. Must be ≤ `max_timeout_ms`. |
| `max_request_bytes` | u64 | `1048576` (1 MiB) | Cap on the outbound request body size. |
| `max_response_bytes` | u64 | `10485760` (10 MiB) | Cap on the response body size. |
| `allow_plaintext` | bool | `false` | Allow `http://` destinations. Default requires HTTPS. |

### `[outbound.rate_limit]`

| Field | Type | Default | Meaning |
|---|---|---|---|
| `rpm` | u32 | — | Per-API egress rate cap (requests per minute). |
| `burst` | u32 or null | `rpm / 60` | Burst capacity. |

```toml
[capabilities]
outbound_http = true

[outbound]
allowed_hosts = ["api.stripe.com", "*.openai.com"]
max_timeout_ms = 15000
default_timeout_ms = 5000
```

---

## `[secrets]`

Declares the names of secrets your service will reference at runtime. Values are bound out-of-band via the admin API (`PUT /_admin/secrets/{owner}/{service}/{name}`); your Wasm code never sees the raw value — only the declared name.

The `[secrets]` table is transparent: each key is a secret name, each value is a spec object.

```toml
[secrets]
stripe_key = { usage = ["outbound-header"] }
openai_key = { usage = ["outbound-header"] }
```

| Spec field | Values | Meaning |
|---|---|---|
| `usage` | `["outbound-header"]` | Where the secret may be used. Currently the only accepted value is `outbound-header` — inject the secret value as an HTTP header in an `outbound_http::fetch` call. At least one entry is required. |

Undeclared secret names return `unknown-secret` at runtime regardless of whether a value is bound.

---

## `[background_jobs.handlers.<name>]`

Declares a job handler that the background-jobs worker can call. Each handler is a separate TOML sub-table. Handler names must start with an ASCII letter and contain only ASCII alphanumerics and `_` (max 64 chars).

Declaring handlers without granting `capabilities.background_jobs = true` is valid (the service processes jobs but never enqueues them). The inverse is also valid (the service enqueues jobs but another service processes them — not typical, but supported).

| Field | Type | Default | Meaning |
|---|---|---|---|
| `deadline_ms` | u32 | `30000` | Max wall-clock time for one handler invocation. Must be > 0. |
| `max_attempts` | u32 | `3` | Retry limit per job. Must be ≥ 1. |
| `backoff_ms` | u32 | `1000` | Delay between retry attempts (ms). |
| `max_concurrent_per_tenant` | u32 or null | `null` (unlimited) | Per-tenant in-flight cap for this handler. Omit for no per-handler limit; the global tenant cap still applies. Must be > 0 when set. |
| `schedule` | string or null | `null` | 6-field cron expression (`sec min hour day month dow`). When set, the host materialises a scheduled job that fires this handler on the given cadence. Example: `"0 0 * * * *"` = top of every hour. |

---

## `[store]`

Reserved section. The platform provides isolated, transactional storage to any service with `capabilities.store = true`. No configuration knobs are available today — the section may be omitted or left empty.

```toml
[store]
# no fields today
```

---

## `[provisioning]`

Advanced: controls who may provision a service instance from a published module (relevant to the Boogy module registry). Most developers deploying their own services can omit this section entirely.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `mode` | string | `"public"` | `"public"` (anyone may provision), `"private"` (only the module author), or `"allowlist"` (only listed user IDs). |
| `allow` | string array | `[]` | User IDs permitted to provision when `mode = "allowlist"`. Required non-empty when mode is `allowlist`. |

---

## Common errors

**Capability used but not granted**

If your Wasm component calls `store::*`, `auth::*`, etc. without the corresponding capability in `[capabilities]`, the host will return an error at the linker stage (before your code runs). Grant the capability in the manifest.

**Path traversal characters rejected**

`service.id` (and `owner`, if you set it) must be ASCII alphanumeric plus `-` and `_`, max 64 chars, no leading `-`. Characters like `/`, `\`, `.`, `:`, and all Unicode are rejected. The validator also rejects `..` outright, and reserved owner names (`v1`, `healthz`, `_admin`, `_agents`, `_sys`).

**`outbound_http` with empty `allowed_hosts`**

Granting `capabilities.outbound_http = true` without a `[outbound]` block (or with `allowed_hosts = []`) fails validation. The capability requires at least one destination.

**`allowlist`/`internal`/`mixed` ingress with empty lists**

`mode = "allowlist"` requires at least one `allowed_agents` entry; `mode = "internal"` requires at least one `allowed_origins` entry; `mode = "mixed"` requires at least one entry in either list. An empty list silently denies every request at runtime — the validator catches this at deploy time.

**`cpu_deadline_ms` out of range**

Must be in the range 1–600000. Zero or values above 600000 are rejected.

**Invalid handler name**

Background-job handler names must start with an ASCII letter and contain only ASCII alphanumerics or `_`, max 64 chars. Names that don't meet this rule are rejected at deploy time.
