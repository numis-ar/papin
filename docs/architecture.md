# Architecture — Remote Papin Agent Harness

Companion to [PLAN.md](PLAN.md) (Revision 5). This document describes the component
structure and the message flows of the backend as implemented in M0+M1.

## 1. Components

```
┌──────────────┐        WireGuard tunnel         ┌────────────────────────────────────────────┐
│  Android app │ ═════════════════════════════════│ Backend: papin-gateway.service (Rust,       │
│(Kotlin/Comp) │   HTTP: WS primary / POST+SSE   │  unprivileged user `papin-gateway`)         │
│  WG tunnel   │   per RFD, scoped per agent     │  axum + tokio + tokio-tungstenite          │
└──────────────┤                                   │ ┌────────────────────────────────────────┐ │
┌──────────────┤  GET  /api/v1/agents              │ │ Agent registry: metadata dir — the     │ │
│ CLI (Rust)   │  GET  /api/v1/config-catalog      │ │ only database (id, name, config,       │ │
│  ratatui     │  POST /api/v1/agents {config}     │ │ state)                                 │ │
└──────────────┤  DELETE /api/v1/agents/{id}       │ ├────────────────────────────────────────┤ │
               │                                   │ │ Rootfs bootstrap engine: clone catalog │ │
               │  ACP v1 at /acp/agents/{id}       │ │ base image → agents/<id>/root (+ACLs,  │ │
               │  (WS / POST+SSE, RFD)             │ │ /etc files, ready-marker)              │ │
               │ ────────────────────────────────→ │ ├────────────────────────────────────────┤ │
               │ ←──────────────────────────────── │ │ Per-agent ACP façade + proxy: envelope │ │
               │                                   │ │ routing, capability policy, reverse-   │ │
               │                                   │ │ RPC forwarding, error mapping          │ │
               │                                   │ ├────────────────────────────────────────┤ │
               │                                   │ │ Agent connection manager: connect() =  │ │
               │                                   │ │ UnixStream + 1-line bootstrap, then    │ │
               │                                   │ │ NDJSON ACP; idle-timeout disconnect    │ │
               │                                   │ └────────────────────────────────────────┘ │
               │                                   │          │ /run/papin-gateway/agent.sock
               │                                   │          ▼ (ONE fixed path; Accept=yes)
               │                                   │   systemd  ┌────────────────────────────┐
               │                                   │   = spawn  │ papin-agent@<n>.service     │
               │                                   │   machinery│ DynamicUser=yes (fresh uid)│
               │                                   │   only     │ wrapper: chroot → agents/  │
               │                                   │            │ <id>/root, exec `kimi acp` │ │
               │                                   │            │ (the jail)                 │ │
               │                                   │            └────────────────────────────┘
               │                                   └────────────────────────────────────────────┘
```

### 1.1 `papin-gateway` (this milestone)

M1 implements, in `rust/crates/papin-gateway`:

| Module | Responsibility |
|---|---|
| `config` | `gateway.toml` (provider selection, fake script path, idle timeout, token TTL), `tokens.toml` (device tokens with `created_at`/`expires_at`). |
| `provider` | `AgentProvider` trait (PLAN §5.2) + `FakeProvider` (M0/M1, no systemd/root) + `SpawnProvider` (M2: UnixStream to the activation socket + one-line bootstrap handshake, error reasons mapped to 404/503). Shared `RegistryDir` metadata-dir registry with strict catalog validation. |
| `manager` | One long-lived NDJSON ACP connection per agent; opened on first client attach; idle timeout (any NDJSON line in either direction resets it; `idle_timeout=0` disables); transparent crash healing (reconnect → re-`initialize` → `session/load` for attached sessions). |
| `proxy` | The per-agent ACP façade: envelope routing by JSON-RPC `id` + `sessionId`, `session/update` forwarding, agent→client reverse-RPC (`session/request_permission`, `session/elicitation`) forwarding and response routing, `session/cancel` and `$/cancel_request` both directions, capability policy, error translation. |
| `auth` | Bearer device-token middleware; expired/unknown token → 401 with actionable JSON message. |
| `bootstrap` | `BootstrapEngine` trait: `FakeBootstrap` (M2) stand-in and `SpawnBootstrap` (M4) — FICLONE reflink clone of catalog base images (probe-once, silent copy fallback, never hardlink), layout/ACL/identity/marker per §5.4. |
| `main` | `papin-gateway` binary: `serve` (provider=fake\|spawn wiring, §5.2) and `mkbase` (catalog base images). M4 adds `/api/v1/status` and `/api/v1/enroll` (§7.1: token→pubkey binding, peers file, setuid helper subprocess). |
| `server` | axum routes: `GET /healthz`, `GET /acp/agents/{id}` (WebSocket upgrade, primary RFD profile), `POST /acp/agents/{id}` (`initialize` → 200+JSON, everything else → 202), registry REST `GET/POST/DELETE /api/v1/agents` + `GET /api/v1/config-catalog` (M2). |

The SSE profile is M6.

### 1.2a Android app (M5)

Kotlin/Jetpack Compose client (`android/`, package `ar.numis.papin`): QR
onboarding with device-side key generation → `POST /api/v1/enroll` →
userspace WireGuard tunnel (`com.wireguard.android:tunnel` GoBackend, no
root); agent-list home screen (state badges, pull-to-refresh,
catalog-driven create dialog); conversation chat with per-output-type
components (bubbles, collapsible thinking cards, tool-call cards, plan
checklists); full-screen question dialogs for reverse-RPC; bottom nav
(Agents / Sessions / Settings); foreground service holding the ACP WS with
notifications for questions and turn completion. Source-complete; SDK build
verification pending (see android/README.md).

### 1.3 `papin-cli` (M3)

Terminal client (§6): agent picker → ratatui chat over the gateway WS ACP
endpoint. The transcript/turn state machine lives in a terminal-independent
`model` (thinking blocks, tool-call cards, plan checklists, permission
answers — unit-tested with canned envelopes); `tui` only renders. Headless
`papin -p` prints the final response (progress as JSONL on stderr);
`papin doctor` runs WG/TCP/WS+initialize/registry checks with actionable
hints; `papin agent new|rm` manage the registry. Reconnect healing: on WS
drop the CLI re-connects, re-initializes, and re-issues `session/load`,
mirroring the gateway-side healing.

### 1.2 `acp-wire` (shared crate)

- Hand-rolled serde types matching the **ACP v1 wire format exactly** (field names per the
  vendored schema 1.23.0 `schema.json`, used as reference only — no path dependency on
  `resources/`, which is a read-only vendored copy): a single `Envelope` type covering
  JSON-RPC request/response/notification with `id`, `sessionId`, `method`, `params`,
  `result`, `error`.
- NDJSON codec: `write_frame` / `read_frame` over any async reader/writer.
- WebSocket client helper (tokio-tungstenite) plus message↔text conversion.

## 2. Sequence — WS full turn (M1 demo path)

```
Client                     Gateway                        Agent (kimi acp / fake)
  │                          │                                │
  │── GET /acp/agents/{id} ─Upgrade: websocket─────────────→│ (auth: Bearer device token)
  │←─────────────── 101 Switching Protocols ────────────────│
  │                          │                                │
  │── {id:1, method:"initialize", params:{protocolVersion:1, ...}} ─┐
  │                          │── forward ─────────────────────→│
  │                          │←── result (capabilities) ───────│
  │←── result, capabilities policy-filtered (no fs/terminal) ─│
  │── {method:"initialized"} │── forward ─────────────────────→│
  │                          │                                │
  │── {id:2, method:"session/new", params:{cwd}} ────────────→│ (routed by id)
  │←── {id:2, result:{sessionId}} ───────────────────────────│
  │                          │  (sessionId → owning client recorded)
  │── {id:3, sessionId, method:"session/prompt", params:{prompt}} →
  │                          │                                │
  │←── session/update (agent_message_chunk, tool_call, ...) ──│ (notifications forwarded)
  │←── {id:3, result:{stopReason:"end_turn"}} ───────────────│
  │                          │                                │
  │── {id:4, sessionId, method:"session/cancel"} ────────────→│
  │←── {id:4, result:{}} ────────────────────────────────────│
```

Key points:

- **Routing by JSON-RPC id.** Every client→agent request registers `id → owning client`
  in the proxy; every agent→client response is forwarded by that map. Agent→client
  reverse-requests (permission/elicitation) register `id → agent` so the client's
  response is routed back to the agent unchanged.
- **Capability policy.** The gateway strips `fs` and `terminal` from the `initialize`
  response's `capabilities` before it reaches the client (all tool execution stays
  inside the chroot); `elicitation` passes through. See protocol-notes.md §3.
- **Idle timeout.** The connection is dropped only after a full idle period with no
  NDJSON line in either direction; an in-flight turn never gets cut.
- **Crash healing.** If the agent stream hits EOF, the gateway reconnects (respawn),
  re-runs `initialize`/`initialized` gateway-side, re-issues `session/load` for every
  attached session, and resumes forwarding. Clients observe at most a pause.

## 3. Sequence — POST profile (M1 subset)

```
Client                     Gateway                        Agent
  │── POST /acp/agents/{id} {id:1, method:"initialize", ...} ─→
  │←── 200 OK {id:1, result:{capabilities,...}} ─────────────│ (policy-filtered)
  │── POST /acp/agents/{id} {id:2, method:"session/prompt",..}─→
  │←── 202 Accepted (empty body) ────────────────────────────│
```

Per the RFD, non-`initialize` POSTs return 202 immediately and the response is delivered
on the SSE GET streams; full SSE streaming is M6 (the RFD requires HTTP/2 for the
POST+SSE profile). As an M1 simplification the gateway forwards the message to the agent
and returns the JSON-RPC response envelope **synchronously** in the 202 body, so POST is
usable for one-shot request/response probing until the SSE profile ships. See
protocol-notes.md §2.

## 4. Testing

- Rust tests run entirely on `FakeProvider` (`rust/scripts/fake-papin-acp`) — no systemd, no
  root (the environment here has a restricted PID namespace; `systemctl
  is-system-running` → degraded).
- Coverage: envelope routing units, capability policy, WS integration against the axum
  server (full turn vs the fake), cancel both directions, idle timeout with paused tokio
  time, crash-healing reconnect + `session/load`, auth (unknown/expired token → 401),
  POST initialize → 200 / non-initialize → 202. M2 adds: registry REST (catalog
  validation, state from the connection table, 409/force-delete), the full session
  surface (`session/list|load|set_mode|set_config_option|close|delete`,
  `session/set_model`, replay burst, cancel races), bootstrap-including-ACLs, and
  the SpawnProvider handshake against a stub unix socket.
