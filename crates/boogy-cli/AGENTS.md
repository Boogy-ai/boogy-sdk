# boogy-cli: command reference for API authors and agents

Canonical reference for the `boogy` CLI — the tool an API author (or
coding agent acting on their behalf) uses to build, deploy, inspect, and
remove Boogy APIs. Read this before invoking any `boogy` command.

If you find a command or flag here that does not match the source under
`crates/boogy-cli/src/`, treat the source as authoritative and note the
discrepancy. The source is the contract.

See also:
- `crates/boogy-sdk/AGENTS.md` — authoring patterns for the API itself
- `CLAUDE.md` — host env vars, manifest sections, ingress modes, auth flows

---

## Install

```bash
cargo install --path crates/boogy-cli        # installs `boogy` binary
cargo run -p boogy-cli -- <command> [args]   # no install needed
```

## Global flag

| Flag | Default | Notes |
|------|---------|-------|
| `--host <url>` | `http://localhost:3000` | Base URL of the Boogy host. Applies to all subcommands. |

No env var shortcut exists in the source — `--host` is the only mechanism.

```bash
boogy --host https://boogy.example.com list
```

---

## Commands

### `boogy build <path>`

Compiles the API crate at `<path>` to `wasm32-wasip2` in release mode.

```bash
boogy build crates/examples/hello-api
```

Runs `cargo build --target wasm32-wasip2 --release` inside `<path>`.
Exits non-zero and prints the cargo error if the build fails.

Output wasm lands at:

```
<path>/target/wasm32-wasip2/release/<crate_name>.wasm
```

For a workspace crate the output is in the **workspace** `target/`,
not the crate's own directory:

```
target/wasm32-wasip2/release/hello_api.wasm
```

The `boogy.toml` manifest's `api.wasm` field must resolve to this path
relative to the manifest file. See the manifest section below.

---

### `boogy deploy <manifest>`

Reads the manifest at `<manifest>`, resolves `api.wasm` relative to the
manifest directory, and posts both to `/_admin/deploy` as a multipart
upload.

```bash
boogy deploy crates/examples/hello-api/boogy.toml

# Against a non-default host:
boogy --host https://boogy.example.com deploy crates/examples/hello-api/boogy.toml
```

The deploy endpoint requires admin scope (see Auth section). Without a
valid bearer token the host returns 401.

On success the host recompiles the wasm, registers the route, and
responds 200. The CLI prints "Deployed successfully!" and echoes any
response body.

On failure the CLI prints the HTTP status and response body then exits
non-zero. The routing table is not updated on failure — the prior
deployment (if any) stays live.

---

### `boogy list`

Lists all APIs currently registered in the host's routing table.

```bash
boogy list
boogy --host https://boogy.example.com list
```

Calls `GET /_admin/services` and pretty-prints the JSON response. Each
entry includes `service_id`, `user_id`, the route path pattern, and the
allowed methods.

`/_admin/services` does **not** require admin scope — it is unauthenticated
and is used as a compose healthcheck. The CLI sends no bearer token for
this command.

Example output:

```json
[
  {
    "service_id": "hello-api",
    "user_id": "alice",
    "path": "/api/hello",
    "methods": ["GET", "POST"]
  }
]
```

---

### `boogy remove <owner>/<api-id>`

Removes a deployed service from the routing table.

```bash
boogy remove alice/hello-api
boogy --host https://boogy.example.com remove alice/notes-api
```

The argument is `<owner>/<api-id>` — a slash-separated pair. The CLI
passes it verbatim to `DELETE /_admin/services/{owner}/{api-id}`. Omitting
the owner segment will produce a 404 or 405 from the host.

Requires admin scope. Returns 200 on success (prints "Removed API:
alice/hello-api") or exits non-zero with the HTTP status on failure.

---

## Auth

**Current state:** the CLI sends **no Authorization header**. Every
`/_admin/*` endpoint (deploy, remove) is protected by `require_admin_scope`
on the host, which returns 401 if no identity is attached and 403 if the
identity lacks `admin` scope.

In the current implementation the workaround is to hit the admin
endpoints via `curl` with a manually-obtained bearer token, or to run
the CLI against a host where the admin middleware is bypassed (e.g. a
local dev host with no auth enforcement — not the default).

**Obtaining an admin token:**

1. Register an agent: `POST /_agents/register` (password or agentkey).
2. Set `BOOGY_BOOTSTRAP_ADMIN_HANDLE=<handle>` on the host before starting;
   the host grants `admin` scope idempotently at startup.
3. Login: `POST /_agents/login/password` → `{"token": "v4.public...."}`
4. Pass via curl: `curl -H "Authorization: Bearer $TOKEN" ...`

Token is PASETO v4.public. Set `BOOGY_AUTH_KEY_FILE` on the host so the
signing key persists across restarts — otherwise all tokens are invalidated
on every restart.

Until the CLI gains native `--token` support, use `curl` for deploy and
remove operations that require auth.

---

## Manifest (`boogy.toml`)

Every deployed service requires a `boogy.toml` next to its crate. The
`boogy deploy` command reads this file directly — pass the path
explicitly; there is no automatic discovery.

```toml
[service]
id = "hello-api"
name = "Hello API"
version = "0.1.0"
wasm = "../../../target/wasm32-wasip2/release/hello_api.wasm"

[api.owner]
user_id = "alice"

[routing]
path = "/api/hello"
methods = ["GET", "POST"]

[capabilities]
store   = true
auth    = true
logging = true

[limits]
memory_mb = 32
timeout_ms = 5000
```

Rules enforced at deploy time:

- `api.id` and `api.owner.user_id` must be ASCII alphanumeric plus `-`/`_`,
  no dots, no leading hyphen, max 64 chars. Reserved: `v1`, `healthz`,
  `_admin`, `_agents`.
- `api.wasm` is resolved relative to the manifest file. Workspace crates
  built from the repo root must traverse up to the workspace `target/`
  (see example above).
- `[capabilities] outbound_http = true` requires a non-empty `[outbound]`
  block with `allowed_hosts`.
- The `[store]` section has no `engine` knob — FoundationDB is the sole
  per-service store engine. The section is reserved for future store config.
  See `CLAUDE.md`.

Full schema: see the Boogy documentation for the authoritative manifest field reference, including ingress modes and delegation.

---

## Typical dev loop

```bash
# 0. Have a Boogy host running (local dev server or a deployed host)

# 1. Create and build the API
cargo new --lib crates/my-api
# ... write lib.rs, boogy.toml ...
boogy build crates/my-api

# 2. Deploy
boogy deploy crates/my-api/boogy.toml

# 3. Smoke test
curl http://localhost:3000/alice/api/my-endpoint

# 4. Iterate: edit → build → deploy
boogy build crates/my-api && boogy deploy crates/my-api/boogy.toml

# 5. Inspect what's live
boogy list

# 6. Tear down
boogy remove alice/my-api
```

Deploy is idempotent: redeploying the same `(owner, service_id)` supersedes
the prior wasm in the routing table and in the PG deployment registry.
The prior wasm blob is kept on disk for rollback; the active route
switches immediately.

---

## Common errors and anti-patterns

| Symptom | Cause | Fix |
|---------|-------|-----|
| `manifest missing api.wasm field` | No `wasm` key in `[service]` | Add `wasm = "..."` to `[service]` |
| `failed to read wasm at: ...` | Wasm not built or wrong path | Run `boogy build` first; verify path in manifest |
| `cargo build failed` | Rust compilation error | Check compiler output |
| `failed to reach host` | Host not running or wrong URL | Start host or fix `--host` |
| `deploy failed (401)` | No bearer token | See Auth section — CLI sends no auth header |
| `deploy failed (403)` | Identity lacks `admin` scope | Grant admin via `BOOGY_BOOTSTRAP_ADMIN_HANDLE` + restart |
| `deploy failed (400)` | Manifest validation failure | Read error body: bad `user_id`/`service_id`, missing `[outbound]`, reserved name |
| `failed to remove API: 404` | Wrong id format or not deployed | Use `owner/api-id`; confirm with `boogy list` |

**Footguns to avoid:**

- `boogy remove alice/hello-api` not `boogy remove hello-api` — the host
  route is `/_admin/services/{owner}/{service_id}`; omitting the owner gives 404.
- Deploying without building — `boogy deploy` uploads whatever wasm is
  currently on disk. Always `build` first when source has changed.
- `BOOGY_HOST_URL` — documented in the CLI README but **not implemented**
  in the source. Use `--host`.
- Ephemeral signing keys — without `BOOGY_AUTH_KEY_FILE` on the host,
  all tokens are invalidated on every host restart.
