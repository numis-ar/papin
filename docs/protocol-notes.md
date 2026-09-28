# Protocol Notes — ACP v1 over Remote Transports

How `papin-gateway` speaks ACP v1 (Zed Agent Client Protocol, schema **1.23.0**) to
clients, where it follows the vendored Streamable HTTP & WebSocket Transport RFD, where it
deliberately deviates, and how agent processes are bootstrapped.

## 1. Wire format

- All ACP traffic is **NDJSON**: one JSON-RPC message per line (`application/json-lines`,
  UTF-8, `\n` delimited). stdio (gateway→agent, via the provider) and WebSocket
  text frames (gateway↔client) carry identical message bytes.
- Messages are the ACP v1 JSON-RPC envelope: request `{id, sessionId?, method, params?}`,
  response `{id, sessionId?, result?|error?}`, notification `{sessionId?, method, params?}`.
  `id` is string or number (or absent for notifications); `sessionId` is a string carried
  both as a top-level JSON-RPC field and inside `params` for session-scoped methods.
- Shared types live in `acp-wire` (`Envelope`, `RpcError`, NDJSON codec). They are
  hand-written against the vendored `schema.json` **as reference**; nothing under
  `resources/` is a build dependency (it is a read-only vendored copy).

## 2. Transport profiles (RFD application + deviations)

The RFD (`docs/rfds/streamable-http-websocket-transport.mdx`, vendored) defines two
profiles on one endpoint. We apply it **under an agent scope**: the endpoint is
`/acp/agents/{id}`, i.e. one logical ACP connection per agent, documented extension.

| Method | Upgrade? | Gateway behavior (M1) |
|---|---|---|
| `GET /acp/agents/{id}` | `Upgrade: websocket` | 101 → persistent full-duplex ACP connection (**primary profile**). RFD-conformant over HTTP/1.1. |
| `POST /acp/agents/{id}` | — | `Content-Type: application/json` required (415 otherwise). `initialize` → **200 + JSON body** (capabilities after policy filter). Any other single JSON-RPC message → **202 Accepted**; the response is routed back only if a stream exists (full SSE is M6 — see below). Batch requests → 501. |
| `GET /acp/agents/{id}` | no | M6 (SSE streams + `Acp-Connection-Id` validation table 400/404/406). |
| `DELETE` | — | M6. |

**RFD deviations / notes (logged per plan §11.7):**

1. **POST+SSE profile is M6-gated.** The RFD *requires HTTP/2* for the POST+SSE profile;
   M1 ships only the WebSocket profile, which is conformant over HTTP/1.1. Until M6,
   non-`initialize` POSTs are accepted (202) and — as an M1 simplification — carry the
   JSON-RPC response envelope synchronously in the response body; the RFD-correct
   delivery on SSE GET streams replaces this in M6.
2. **Agent-scoped endpoint.** The RFD uses one `/acp` endpoint per server; we scope per
   agent (`/acp/agents/{id}`) because the gateway is a multi-agent façade. Connection
   identity is thereby implied by the URL path; the `Acp-Connection-Id` header machinery
   (and client cookie requirements) only becomes relevant when the SSE profile ships in
   M6. WS-profile clients are not required to handle cookies.
3. **One gateway connection per agent.** Internally the gateway keeps a single
   long-lived agent connection and multiplexes attached clients onto it. In ACP terms
   each agent-side connection hosts the sessions created over it; the gateway tracks
   `sessionId → owning client` so reverse-RPC and notifications reach the right client.
4. **Error surface for unhealable agent death.** RFD v1 leaves durability to the
   implementer; we implement transparent crash healing (§4 below) and only surface a
   structured client error after repeated respawn failure.

## 3. Capability policy

The gateway is a thin ACP proxy: an agent façade toward clients, a client toward
`kimi acp`. `kimi acp` advertises `fs` and `terminal` capabilities (its SDK executes
those tools locally); advertising them to remote clients would let a stolen device token
execute arbitrary file/terminal operations on the agent host. Policy (applied to the
`initialize` **response** only):

- **Strip** `capabilities.fs` and `capabilities.terminal`.
- **Pass through** everything else, including `elicitation` (questions are answered by
  the owning client) and provider extensions (e.g. the Kimi SDK's `session/set_model`, `auth`).
- The agent-side stream is untouched: `kimi acp` still sees its own capabilities echoed
  by the gateway's gateway-side `initialize`, so in-chroot tool execution keeps working.

Error translation (§5.3): agent errors carrying `auth_required` are rewritten into an
actionable client error ("re-authenticate the shared kimi login; contact the
administrator"); `turn.agent_busy` passes through unchanged; an unhealable agent EOF
yields a structured error (`code: -32000, message: "agent connection lost"`) plus the
last lines of the agent journal once journal access exists (M4).

### 2.1 Registry + catalog REST (M2, PLAN §5.1)

- `GET /api/v1/agents` → `[{id, name, config, created_at, state}]`; `state` is
  gateway-side: `active` iff a client connection is attached, `error` when the
  last connect attempt failed with nobody attached, else `stopped`.
- `GET /api/v1/config-catalog` → `{bases, seeds, env}` — the only values
  `POST /api/v1/agents` accepts; unknown base/seed/env keys → 400.
- `POST /api/v1/agents {id?, name?, config: {base, seed?, env?}}` → validate id
  (`^[a-z0-9][a-z0-9-]{0,63}$`) and catalog strictly, write the metadata record
  (temp file + atomic rename), bootstrap the rootfs, return 201. Duplicate id →
  409. No process starts.
- `DELETE /api/v1/agents/{id}` → 409 while a connection is attached;
  `?force=true` detaches all clients first; removes metadata + rootfs tree.
- Records are `<state_dir>/agents/<id>.json`; the registry is the only database.

### 2.2 Session surface (M2)

All ACP v1 session methods are envelope-routed passthroughs:
`session/new|load|prompt|cancel|set_mode|set_config_option|close|delete|list`,
plus the provider extension `session/set_model`. `session/load` transfers session
ownership on response; the replay burst (`session/update` notifications
preceding/following the load response) routes to the owning client by
`sessionId`. Cancel races (cancel vs. in-flight turn, cancel vs. outstanding
permission reverse-RPC) resolve to a definite outcome: the cancel is always
acked and the prompt returns either its result or a cancellation error.

## 4. Agent connection lifecycle (plan §5.2/§5.3)

### 4.1 SpawnProvider bootstrap handshake (production, M4)

Before any ACP bytes flow, gateway ⇄ wrapper exchange one NDJSON line each:

1. gateway → wrapper: `{"agent":"<id>"}` (validated by the wrapper against
   `^[a-z0-9][a-z0-9-]{0,63}$`).
2. wrapper → gateway: `{"status":"ready"}` on success; on failure
   `{"status":"error","reason":"unknown_agent"|"rootfs_not_ready"|"exec_failed"}`.
3. Gateway maps `unknown_agent` → client-facing **404**, `rootfs_not_ready` /
   `exec_failed` → **503**. No ACP is proxied until `ready`. The `.papin-rootfs-ready`
   marker (dropped last, by atomic rename, at rootfs bootstrap) prevents entering a
   half-built rootfs.

### 4.2 SpawnProvider (M2)

`SpawnProvider::connect()` = `UnixStream::connect(socket_path)` + the bootstrap
handshake (§4.1), then raw NDJSON ACP. Implemented and unit-tested against a
stub socket (ready + each error reason); the chroot wrapper (`papin-acp-enter`)
itself remains M4.

### 4.3 FakeProvider (M0, dev/tests)

`provider = "fake"` in `gateway.toml`. Registry lives in a plain directory
(`<state_dir>/agents/<id>.json`); `connect()` spawns `rust/scripts/fake-papin-acp` on a tokio
duplex pair with **no bootstrap handshake** (instantly ready). Used by all M1–M3 tests.

### 4.4 Connection manager semantics

- **One long-lived NDJSON connection per agent**, opened on first client attach, closed
  after an idle timeout (default 10 min, `idle_timeout_secs`; `0` = keep forever).
  **Any NDJSON line in either direction resets the timer** — an in-flight turn (steady
  `session/update` stream) is never cut; only true quiescence times out.
- **Crash healing is transparent.** On agent EOF the gateway: re-`connect()` (socket
  activation respawns the instance; FakeProvider respawns the script) → gateway-side
  `initialize`/`initialized` → `session/load` for every session that was attached →
  resume envelope forwarding. Clients observe at most a pause; ACP v1 `session/load`
  replay (summary + recent updates) restores the conversation. Repeated spawn/handshake
  failure is surfaced as a structured client error.
- **Reverse-RPC.** Agent→client requests (`session/request_permission`,
  `session/elicitation`) are forwarded to the owning client by `sessionId`; the client's
  response is routed back by JSON-RPC `id`. If the owning client disconnects mid-RPC,
  the gateway answers the agent with a structured `request_cancelled` error after a
  short grace (the in-flight turn then typically ends with `turn.agent_busy` or an
  error, which is passed through).
- **Cancel both directions.** `session/cancel` (client→agent) and `$/cancel_request`
  (either direction) are forwarded verbatim; ids are routed like any other request.
- **One connection per agent internally** — overlapping connects against a dying
  instance are serialized with backoff.

## 5. Auth

- Every route except `GET /healthz` requires `Authorization: Bearer <device-token>`.
- Tokens are provisioned into `tokens.toml` (by `add-peer`, M4); each entry carries
  `created_at` + `expires_at`; lifetime is server-defined (`token_ttl` in
  `gateway.toml`, applied to entries that lack `expires_at`).
- Unknown or expired token → **401** with an actionable JSON body
  (`{"error":"unauthorized","message":"device token unknown or expired; re-run add-peer ..."}`).

## 6. Rootfs bootstrap (M2 stand-in)

`BootstrapEngine` is the abstraction; `FakeBootstrap` builds the dev/test
stand-in under `agents/<id>/root`: writable dirs (`home/papin` 0700, `tmp`
1777, `workspace`), setgid + best-effort `setfacl` default ACLs
(`group:papin-gateway:rwx`, skipped silently when the tool/fs is missing, like
reflink), identity files (`etc/{passwd,group,nsswitch.conf,hosts,resolv.conf}`)
from templates, workspace seed copy (never hardlink — clones must not share
inodes with templates), and `.papin-rootfs-ready` dropped last via atomic
rename. `SpawnBootstrap` (reflink clone of catalog base images, real ACLs) is
stubbed until M4.
