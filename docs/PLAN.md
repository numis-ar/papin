# PLAN — Remote Kimi Agent Harness (Android app + Backend + CLI)

> Revision 5 — open questions from Rev 4 resolved (decisions logged in §11); corrections:
> vendored schema crate is **1.23.0** (not 1.9.1); the Streamable HTTP/WS RFD is a
> draft doc, not schema-encoded — conformance is by RFD text; the RFD **requires
> HTTP/2** for the POST+SSE profile, so SSE conformance is M6-gated while the
> WebSocket profile (primary) is conformant over HTTP/1.1.
>
> Revision 4 — systemd is **spawn machinery only**, never managed:
> one static socket-activated definition (`kimi-agent.socket` + `kimi-agent@.service`) spawns any
> number of agents; a capability-scoped wrapper chroots into a per-agent rootfs that the **gateway
> bootstraps** from client-selected config; every agent gets its own chrooted filesystem with a
> **DynamicUser**; the gateway holds **no systemd permissions** (no polkit, no D-Bus) — connecting
> to the socket is what spawns an agent. Backend and CLI in Rust.

## 1. Goal

1. **Android app** — behaves like the Kimi Code TUI: send a prompt, watch the agent work (thinking, tool calls, commands), answer questions, read the final response.
2. **Backend (`kimi-gateway`, Rust)** — a systemd service and WireGuard VPN endpoint. It exposes an **agent registry**: N named agents, each = one `kimi acp` process in its own chrooted filesystem with a dynamic user, spawned by socket activation. Clients list agents, connect to one, or create a new one (gateway bootstraps its filesystem from client-selected configuration).
3. **CLI (`kimi-cli`, Rust)** — same features as the Android app, in the terminal.

Protocol end-to-end is **ACP v1** (Zed Agent Client Protocol). WireGuard carries all client↔backend traffic.

## 2. What the resources give us (verified)

| Resource | What it actually is | How we use it |
|---|---|---|
| `resources/agent-client-protocol` | Zed's ACP repo: canonical JSON Schema v1 (Rust crate `agent-client-protocol-schema` **1.23.0**; v2 crate `2.0.0-alpha.5`), draft **Streamable HTTP & WebSocket Transport RFD**. | The protocol everywhere. The RFD is a draft *document* — it is **not encoded in the schema crate**; transport conformance is implemented from the RFD text (`docs/rfds/streamable-http-websocket-transport.mdx`) as vendored. The RFD requires **HTTP/2** for the POST+SSE profile (so that profile is M6-gated); the WebSocket profile is conformant over HTTP/1.1 and is primary. |
| `resources/kimi-code` | Kimi Code CLI, TS monorepo; `kimi acp` = ACP **agent** over stdio NDJSON (pins `@agentclientprotocol/sdk@^1.3.0`, protocol v1). One process = many multiplexed ACP sessions; one turn in flight per session; questions via agent→client reverse-RPC; unadvertised `fs`/`terminal` caps ⇒ tool execution local to the `kimi acp` host; sessions persisted on disk under `KIMI_CODE_HOME`. | Runs inside each chrooted agent instance, stdio bridged to the activation socket by systemd. |
| `resources/Agent-To-Agent-Protocol` | A2A v1.0. | Future interop only (Agent Card exposure). |
| `resources/wireguard-android` | Official WG Android app; reusable tunnel lib `com.wireguard.android:tunnel:1.0.20260315` on Maven Central (`GoBackend`, userspace, no root). | Android VPN. |
| `resources/wireguard-tools` | Official C tools (`wg`, `wg-quick`) + embeddable netlink library; userspace fallback `wireguard-go` (Go). | Server VPN: `wg-quick@wg0.service` (standard systemd unit). |

### 2.1 Environment facts (verified on this machine)

- `rustc`/`cargo` 1.93.1; crates.io works via sparse index/static (cargo fetch succeeded). Note: the *vendored* schema crate in `resources/agent-client-protocol/schema/v1` is **1.23.0** — pin that version in the workspace, not 1.9.1.
- systemd **259** present (`systemctl is-system-running` → `degraded`; PID namespace is restricted, so full socket-activation testing needs a normal systemd host/VM — see §9), `wg`/`wg-quick` installed, Java 25.
- **Socket-activation pattern (no privileges needed to spawn):** one static `kimi-agent.socket` (`ListenStream=/run/kimi-gateway/agent.sock`, `Accept=yes`) + one `kimi-agent@.service`. Each gateway `connect()` makes systemd spawn a fresh instance with `DynamicUser=yes`, stdio = the connection (`StandardInput=socket` / `StandardOutput=socket`). The instance's `ExecStart` is a tiny capability-scoped **wrapper** that reads a one-line bootstrap (agent id) from the socket, `chroot()`s into the agent's pre-built rootfs, and execs `kimi acp`. No `systemctl`, no polkit, no D-Bus — **connecting is the spawn request**.

## 3. Architecture

```
┌──────────────┐        WireGuard tunnel         ┌────────────────────────────────────────────┐
│  Android app │ ═════════════════════════════════│ Backend: kimi-gateway.service (Rust,       │
│(Kotlin/Comp) │   HTTP: WS primary / POST+SSE   │  unprivileged user `kimi-gateway`)         │
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
               │                                   │          │ /run/kimi-gateway/agent.sock
               │                                   │          ▼ (ONE fixed path; Accept=yes)
               │                                   │   systemd  ┌────────────────────────────┐
               │                                   │   = spawn  │ kimi-agent@<n>.service     │ concurrent
               │                                   │   machinery│ DynamicUser=yes (fresh uid)│ instances,
               │                                   │   only     │ wrapper: chroot → agents/  │ one per
               │                                   │            │ <id>/root, exec `kimi acp` │ agent conn
               │                                   │            │ (the jail)                 │
               │                                   │            └────────────────────────────┘
               │                                   └────────────────────────────────────────────┘
```

**Key properties**

- **Agents are gateway objects, not systemd objects.** The registry is a metadata directory (`/var/lib/kimi-gateway/agents/<id>.json`); systemd sees only anonymous per-connection instances (`kimi-agent@<n>`) that come and go with connections. Listing agents never touches systemd.
- **Spawning = connecting.** The gateway opens the one fixed socket; systemd socket-activation spawns a jailed instance. Crash → EOF → instance exits; next connect respawns. Gateway restart → reconnect → kimi re-attaches to its **disk-persisted** sessions (ACP v1 gives no stream replay; `session/load` after reconnect is the healing path).
- **Every agent is chrooted with a dynamic user.** The gateway bootstraps each agent's rootfs (`/var/lib/kimi-gateway/agents/<id>/root`) by cloning a catalog base image (reflink where supported, else copy), seeding the workspace, writing `/etc/{passwd,group,resolv.conf,hosts,nsswitch.conf}`, setting ACLs, and dropping a ready-marker. The wrapper enters it before kimi starts.
- **The gateway is a thin, per-agent ACP proxy** (agent façade toward clients, client toward `kimi acp`): envelope routing by JSON-RPC `id`/`sessionId`, capability policy (`fs`/`terminal` stripped → all tool execution inside the chroot; `elicitation` passed through), error mapping, passthrough of kimi extensions (e.g. `session/set_model`).
- **Systemd** in build, test, or deploy. Everything runs natively under systemd; dev machines without full systemd use a `FakeProvider` (see §5.2).

## 4. Repository layout

```
/home/kimi/kimi-client/
├── docs/
│   ├── PLAN.md                ← this file
│   ├── architecture.md        ← component + sequence diagrams (M1)
│   ├── protocol-notes.md      ← RFD deviations, capability policy, bootstrap/wrapper protocol (M1)
│   └── operations.md          ← install, agents, base images, WG peers, kimi login, troubleshooting (M4)
├── rust/                      ← cargo workspace
│   ├── Cargo.toml
│   ├── crates/
│   │   ├── acp-wire/          # shared: NDJSON codec, JSON-RPC envelopes (schema crate),
│   │   │                        # WS client/server transport, POST+SSE transport, reconnect+resume
│   │   ├── kimi-gateway/      # backend: /acp/agents/{id} transport (RFD), agent façade proxy,
│   │   │                        # connection manager, registry, rootfs bootstrap engine, ops routes
│   │   ├── kimi-acp-enter/    # the wrapper: bootstrap line → chroot(rootfs) → exec kimi acp
│   │   ├── kimi-enroll-helper/# setuid-root enrollment binary: validate peers file → wg syncconf
│   │   └── kimi-cli/          # CLI binary: ratatui TUI over acp-wire
│   └── tests/                 # cross-crate integration tests (FakeProvider-based)
├── systemd/
│   ├── kimi-gateway.service           # backend unit: unprivileged user, hardened
│   ├── kimi-agent.socket              # ONE static socket: /run/kimi-gateway/agent.sock,
│   │                                  # Accept=yes, SocketGroup=kimi-gateway, SocketMode=0660
│   └── kimi-agent@.service            # ONE definition for all agents: stdio=socket, DynamicUser,
│                                      # ambient caps (SYS_CHROOT, DAC_OVERRIDE), sandbox directives
├── packaging/
│   └── install.sh             # units + wrapper + dirs + user; enables kimi-gateway.service
├── scripts/
│   ├── fake-kimi-acp          # dev harness: ACP NDJSON over stdio with canned events
│   ├── add-peer               # WG peer + device token provisioning (wg syncconf)
│   └── e2e.sh                 # native E2E: gateway + fake/real agent + CLI (no containers)
└── android/
    └── app/                   # Kotlin, Gradle, Jetpack Compose (§7)
```

Runtime dirs on the server: `/etc/kimi-gateway/` (`gateway.toml`, `tokens.toml`), `/run/kimi-gateway/` (the spawn socket), `/var/lib/kimi-gateway/` (`agents/<id>.{json,env}` + `agents/<id>/root` chroot trees, `rootfs/base/<name>` catalog images, `workspaces` seeds).

## 5. Backend: `kimi-gateway` (Rust)

**Stack:** tokio, axum, tokio-tungstenite, serde_json, `agent-client-protocol-schema` 1.23.0, tracing. No D-Bus client — the gateway never talks to systemd at runtime.

### 5.1 Client-facing API

- **Agent registry (small REST, JSON):**
  - `GET /api/v1/agents` → `[{id, name, state: stopped|active|error, config, created_at}]` — read from the metadata dir; `state` from the gateway's own connection table (an agent is `active` iff a client/agent connection is attached).
  - `GET /api/v1/config-catalog` → what clients may select: base images (`node22-kimi-x.y`, …), workspace seeds, allowed env keys with defaults. **Clients choose from the catalog only** — the catalog is the security boundary on creation.
  - `POST /api/v1/agents` `{id?, name?, config: {base, seed?, env?}}` → validate against catalog, write metadata, **bootstrap the rootfs**, return the record. No process starts yet — first connect spawns it.
  - `DELETE /api/v1/agents/{id}` → refuse while a connection is attached (or force-disconnect with `?force=true`); remove metadata + rootfs.
- **ACP transport per agent** (RFD, applied under an agent scope — documented extension):
  - `GET /acp/agents/{id}` + `Upgrade: websocket` → persistent ACP connection (primary). WebSocket profile is RFD-conformant over HTTP/1.1.
  - `POST /acp/agents/{id}` → `initialize` w/o conn id → **200+JSON**; others **202**; `GET` (no upgrade) → SSE connection-scoped / session-scoped streams via `Acp-Connection-Id` + `Acp-Session-Id`; `DELETE` terminates. Same validation table as the RFD (400/404/406/415/501). **The POST+SSE profile requires HTTP/2 per the RFD — it ships in M6 with h2; until then only the WS profile is offered.** Clients on the HTTP profile MUST store and return cookies (RFD requirement; needed for session affinity).
- **Ops:** `GET /healthz`, `GET /api/v1/status` (versions, protocol v1, agent counts, WG iface state), `POST /api/v1/enroll` (WG peer enrollment from QR onboarding — see §7.1).
- **Auth:** `Authorization: Bearer <device-token>` middleware on everything (tokens provisioned by `add-peer` into `tokens.toml`). Tokens carry `created_at` + `expires_at`; lifetime is **server-defined** (`token_ttl` in `gateway.toml`). An expired or unknown token → **401** with an actionable message; rotation = re-run `add-peer`, which reissues the peer's token (atomically rewriting `tokens.toml`).

### 5.2 Agent lifecycle — `AgentProvider` abstraction

```rust
#[async_trait]
trait AgentProvider: Send + Sync {
    async fn list(&self) -> Result<Vec<AgentRecord>>;              // metadata dir
    async fn create(&self, spec: AgentSpec) -> Result<AgentRecord>; // validate vs catalog; write metadata; bootstrap rootfs
    async fn remove(&self, id: &str, purge: bool) -> Result<()>;   // metadata + rootfs (pure filesystem)
    async fn connect(&self, id: &str) -> Result<Box<dyn AsyncReadWrite>>; // ACP byte stream
}
```

- **`SpawnProvider` (production):** `create`/`remove` are **pure filesystem operations** (no systemd contact). `connect()` = `tokio::net::UnixStream::connect("/run/kimi-gateway/agent.sock")` → socket activation spawns a fresh instance → **bootstrap handshake** (see below) → then proxies raw NDJSON ACP; kimi sees a clean ACP stream.
- **Bootstrap handshake** (gateway ⇄ wrapper, before any ACP bytes):
  1. gateway → wrapper: one line `{"agent":"<id>"}`.
  2. wrapper → gateway: one line `{"status":"ready"}` on success, or `{"status":"error","reason":"unknown_agent"|"rootfs_not_ready"|"exec_failed"}`.
  3. Gateway maps `unknown_agent` → client-facing 404, `rootfs_not_ready`/`exec_failed` → 503; no ACP is proxied until `ready`. The ready-marker file prevents entering a half-built rootfs; the handshake prevents proxying into a dead instance.
- **`FakeProvider` (dev/tests, no systemd/root):** registry in a temp dir; `connect()` spawns `scripts/fake-kimi-acp` (or the real `kimi acp`) on a tokio duplex pair, **no handshake** (behaves as instantly `ready`). Selected via `gateway.toml` (`provider = "fake"`). All M1–M3 and most tests run on this.

### 5.3 Agent connection manager & proxy

- One **long-lived NDJSON ACP connection per agent**, opened on first client attach, closed after an idle timeout (default 10 min, config) — socket activation makes respawn ~instant. **Any NDJSON line in either direction resets the idle timer**, so an in-flight turn (which produces a steady stream of `session/update` notifications) never gets cut; the timeout only fires on a truly quiescent agent. `idle_timeout=0` keeps the connection alive indefinitely.
- **Transparent crash healing** (the client never sees agent death): if the agent connection hits EOF mid-session (wrapper exit, `kimi acp` crash, instance kill), the gateway does not propagate a fatal error to attached clients. Instead it re-`connect()`s (socket activation respawns the instance), re-runs `initialize`/`initialized`, re-issues `session/load` for every session that was attached, and resumes envelope forwarding. Clients observe at most a pause; `session/load` replay (summary + recent updates, native ACP v1 behavior) restores the conversation on resume. Repeated spawn failure (handshake error) is surfaced as a structured client error.
- **Proxy engine:** envelope routing by JSON-RPC `id` + `sessionId`; `session/update` notifications → WS frames / session-scoped SSE; agent→client reverse-requests (permission, elicitation) forwarded onto the owning client connection, responses routed back by `id`; if the owning client disconnects mid-reverse-RPC, the gateway answers the agent request with a structured error (request cancelled) after a short grace; `session/cancel` and `$/cancel_request` both directions; capability policy strips `fs`/`terminal`, passes `elicitation`; error translation (`auth_required` → actionable client error; `turn.agent_busy` passed through; unhealable EOF → structured error + journal tail).
- The gateway enforces **one connection per agent** internally (serialize connect with backoff on the rare overlap with a dying instance).

### 5.4 Jail: one definition, per-agent chroot, dynamic user

`systemd/kimi-agent.socket`:

```ini
[Socket]
ListenStream=/run/kimi-gateway/agent.sock
Accept=yes
SocketGroup=kimi-gateway
SocketMode=0660
DirectoryMode=0755
MaxConnections=64
```

`systemd/kimi-agent@.service` (the ONE agent definition):

```ini
[Unit]
Description=Kimi ACP agent (connection %i)

[Service]
Type=exec
ExecStart=/usr/local/lib/kimi-gateway/kimi-acp-enter
StandardInput=socket
StandardOutput=socket
StandardError=journal
# ── identity ──
DynamicUser=yes                 # fresh random uid per spawned instance — per-agent uid separation
NoNewPrivileges=yes
CapabilityBoundingSet=CAP_SYS_CHROOT CAP_DAC_OVERRIDE
AmbientCapabilities=CAP_SYS_CHROOT CAP_DAC_OVERRIDE
# ── sandbox (pre-chroot host view; the chroot is the real jail) ──
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictNamespaces=yes
LockPersonality=yes
RestrictRealtime=yes
SystemCallArchitectures=native
SystemCallFilter=@system-service chroot chdir
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
UMask=0022
# ── resource caps (uniform for all agents) ──
MemoryMax=4G
TasksMax=256
Restart=no                      # EOF (gateway disconnect) is the clean-exit signal
TimeoutStopSec=10
```

**The wrapper `kimi-acp-enter`** (small static Rust bin, lives on the host fs):

1. Read the single bootstrap line from fd 0 (`{"agent":"<id>"}`); validate `^[a-z0-9][a-z0-9-]{0,63}$`.
2. Stat `/var/lib/kimi-gateway/agents/<id>/root/.kimi-rootfs-ready`; on any failure up to this point, write `{"status":"error","reason":"unknown_agent"|"rootfs_not_ready"}` to fd 1 and exit.
3. `chroot(root)` + `chdir("/")` (ambient `CAP_SYS_CHROOT`; no `CAP_SYS_ADMIN` anywhere); on failure, write the error handshake line and exit.
4. Read env from the agent's `.env` metadata (pre-chroot), build a minimal environ (`HOME=/home/kimi`, `KIMI_CODE_HOME=/home/kimi/.kimi-code`, `PATH=…`).
5. Write `{"status":"ready"}` to fd 1 (see §5.2 bootstrap handshake), then `execve("/usr/local/bin/kimi-acp")` — a rootfs-internal shim that execs `kimi acp` (node). If `execve` fails, write `{"status":"error","reason":"exec_failed"}` and exit.
6. Post-exec the process holds only the two ambient capabilities on a minimal bounding set, inside the chroot, as an unprivileged random uid — it cannot see host paths at all.

**Rootfs bootstrap (gateway, at `POST /api/v1/agents`):**

- **Clone** the selected catalog base image `/var/lib/kimi-gateway/rootfs/base/<name>/` → `agents/<id>/root`: reflink (`FICLONE`) where the fs supports it, full recursive copy otherwise — reflink is **best-effort, never a failure**: probe `FICLONE` once at bootstrap and fall back to copy silently. **Never hardlink** (agents must not mutate shared inodes). Base images contain: minimal OS tree, node runtime, kimi install, CA certs — **no setuid binaries, no package manager, no systemd**. operations.md documents btrfs/xfs/ext4+reflink as preferred filesystems (cheap clones); plain ext4 works at full-copy cost.
- **Writable dirs**: `/home/kimi` (state/credentials, 0700+ACL), `/workspace` (agent work), `/tmp` (1777); default ACL `group:kimi-gateway:rwx` + setgid on `/workspace` and `/home/kimi` so the gateway can later clean up files created by the ephemeral DynamicUser (which needs `CAP_DAC_OVERRIDE` at runtime — granted in the unit).
- **Identity files** from templates: `/etc/passwd`, `/etc/group`, `/etc/nsswitch.conf`, `/etc/hosts`, `/etc/resolv.conf` (gateway refreshes these from the host at bootstrap; TTL-refresh on connect).
- Drop `.kimi-rootfs-ready` last (atomic: rename).
- **Workspace seed**: copy a catalog seed dir into `/workspace` — seeds are **client-selectable templates** enumerated by `GET /api/v1/config-catalog` (v1: server-local template dirs only — no network clones; agent egress is LLM API only).

**Network:** agents need outbound HTTPS to the Kimi API; `RestrictAddressFamilies` as above; optional `IPAddressAllow=` tightening in M6. **All agents share one common kimi login** (same credentials/env everywhere): the shared kimi config/credentials live in the base image (built once via one `kimi` login at image-build time) and are copied into each rootfs at bootstrap. No per-agent login flow and no per-agent credentials in v1; the agent `.env` metadata remains but defaults come from the catalog/base image.

### 5.5 Backend service unit

`kimi-gateway.service` runs as a **dedicated unprivileged user** `kimi-gateway` (created by `install.sh`; root is used only at install time):

- **Zero systemd runtime privileges:** spawning agents never involves `systemctl`/D-Bus/polkit — it is a `connect(2)` to the socket. The gateway's systemd involvement is being started *by* systemd.
- **Filesystem:** owns `/var/lib/kimi-gateway/` (metadata, rootfs trees, catalog); runtime socket access via group (`SocketGroup=kimi-gateway`); journal tails via group `systemd-journal`.
- **Its own sandbox:** `ProtectSystem=yes`, `NoNewPrivileges=yes`, `PrivateTmp=yes`, `ProtectHome=yes`, `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`. Not `ProtectNetwork` (it serves WG clients), no ambient capabilities.
- WireGuard runs via the standard `wg-quick@wg0.service` (managed by the admin/`add-peer`, not by the gateway — the gateway holds no `CAP_NET_ADMIN`).

## 6. CLI: `kimi-cli` (Rust)

- **Agent selection first:** `kimi` → agent picker (`GET /api/v1/agents`); `kimi --agent <id>` to skip; `kimi agent new <name> [--base node22-kimi-x.y] [--seed …]` pulls `GET /api/v1/config-catalog` for choices, `kimi agent rm <id>`. Creating then auto-connects.
- TUI as before: streaming transcript (thinking collapse, tool cards, plan checklist), question/permission modals, `Esc` cancel, sessions (`session/list`, `--resume <id>` with `session/load` replay), `/model` `/mode`, `-p` headless, reconnect+load healing.
- `kimi doctor`: WG interface up → TCP reachability → WS handshake → `initialize` → registry reachable; prints `wg-quick up wg0` hint when needed.
- Config `~/.config/kimi-cli/config.toml`: gateway base URL (`ws://10.77.0.1:8080`), device token, default agent.

## 7. Android app (Kotlin)

**UX direction (decided):** conversation-like chat app (not terminal-emulation), Material 3 with a dark theme.

- WireGuard tunnel lib + WS ACP client as before.
- **Agent list is the home screen**: `GET /api/v1/agents` (state badges only — no last-activity/model extras), create dialog driven by `GET /api/v1/config-catalog` (base image, workspace template), tap → connect WS at `/acp/agents/{id}` → chat screen. Pull-to-refresh reflects gateway-side agent states.
- **Chat screen:** every output type gets its own UI component — user question/answer bubbles, **collapsible thinking cards**, **tool-call cards** (collapsed by default, tap to expand args/result), agent message bubbles, plan checklists. Streaming updates animate in place.
- **Questions/permissions:** answered in a **full-screen dialog** (not a bottom sheet) presenting the elicitation/permission request with clear actions; tapping a background notification opens this dialog directly on the right chat.
- **Input:** multiline text field (newline-friendly, send button).
- **Navigation:** bottom nav — Agents / Sessions / Settings.
- **Background behavior:** while backgrounded, the app keeps the ACP WebSocket alive in a **foreground service** (persistent notification) and posts notifications for agent→client reverse-RPC (elicitation/permission questions) and turn completion, so a question never stalls silently. Tapping a notification returns to the affected chat.
- **Onboarding (QR provisioning, decided):** `add-peer` on the server generates a **provisioning link + QR code** containing everything the client needs except secrets: gateway endpoint (WG host:port), server WireGuard public key, gateway tunnel IP/port, assigned client tunnel IP, and the device token. **The WireGuard keypair is generated on the client** — the QR never carries a private key. Flow: scan QR → app generates its keypair → app calls the gateway **enrollment endpoint** (`POST /api/v1/enroll`, bearer = device token from the QR) with its client public key → gateway installs the WG peer (`wg syncconf`) → app imports the tunnel config and connects. Possession of the QR = possession of the token.

### 7.1 Enrollment endpoint (gateway)

- `POST /api/v1/enroll` `{client_pubkey}` — bearer device token (from the QR). Gateway validates the token, renders a complete peers file into its own directory (`/run/kimi-gateway/wg-peers/`), and invokes the **enrollment helper** (below) to install it. Returns `{assigned_ip, gateway_ip}`. A token already bound to a pubkey may be overwritten (device re-keying).
- `add-peer` (server-side script) mints the token + assigned IP, writes `tokens.toml`, and prints the provisioning link/QR — it does **not** touch `wg0`; first enrollment (or re-enrollment) does.

**`kimi-enroll-helper` — a separate, dedicated setuid-root binary** (its own crate; the gateway process itself stays fully unprivileged):

- Owned by root, mode `4755`, installed to `/usr/local/lib/kimi-gateway/kimi-enroll-helper` by `install.sh`. **The gateway never holds `CAP_NET_ADMIN` or any setuid privilege** — it runs this binary as a subprocess, passing only a peers-file path.
- Interface is deliberately tiny: `kimi-enroll-helper apply <peers-file>`. It does nothing else — no other subcommands, no config, no network.
- Hardening by construction: static Rust, no crates beyond std; `setuid(0)` → validates argv (exactly `apply`, one path) → verifies the path is a **regular file inside `/run/kimi-gateway/wg-peers/`** (canonicalizes, rejects symlinks/escapes) → validates the file contents (strict `wg-quick`-style peer stanzas: base64 pubkeys, CIDR IPs only) → `fork`+`execve("/usr/bin/wg", "syncconf", "wg0", path)` with a fixed environment and `NoNewPrivileges` semantics post-exec → prints `ok`/`error` on stdout for the gateway. No shell, no PATH lookup (absolute execve), env scrubbed.
- Layering: the binary is the *only* root-capable component in the system; the socket `0660 kimi-gateway` + bearer-token checks in the gateway sit in front of it, and its input validation is defense-in-depth behind that.
- Documented (threat model + audit notes) in operations.md.

## 8. Build order / milestones

- **M0 — Workspace + dev harness.** cargo workspace; `scripts/fake-kimi-acp`; `FakeProvider`; Android skeleton (placeholder only — real Android work starts at M5). `cargo test` baseline.
- **M1 — Gateway core.** UnixStream/duplex NDJSON client; connection manager; axum WS at `/acp/agents/{id}` + `POST /acp/agents/{id}` (initialize 200); envelope proxy; `session/new|prompt` passthrough vs FakeProvider. Demo: WS client full turn. Docs: architecture.md, protocol-notes.md. Tests: routing units, WS integration, proxy-vs-fake.
- **M2 — Session surface + registry.** `session/update` streaming; reverse-RPC forwarding; cancel both directions; load/resume/list/close/delete/set_mode/set_config_option; metadata-dir registry (`GET/POST/DELETE /api/v1/agents`) + catalog endpoint. Tests: replay burst, cancel races, crash mid-turn, bootstrap-including-ACLs.
- **M3 — CLI.** ratatui TUI + agent picker + `kimi agent new/rm`; full chat; sessions; headless; doctor. Demo vs FakeProvider. Tests: snapshots, PTY script.
- **M4 — systemd + chroot jail + real kimi + WireGuard (normal systemd host/VM).** `kimi-agent.socket`/`kimi-agent@.service`; `kimi-acp-enter` wrapper; bootstrap engine (reflink/copy, identity files, ACLs, marker); `kimi-gateway mkbase` for catalog images; `packaging/install.sh`; gateway service; `wg-quick@wg0` + `add-peer` + `kimi-enroll-helper` (setuid, §7.1); real `kimi acp` + one-time login; `scripts/e2e.sh` native smoke; operations.md. Verify the **stdin-EOF-exit assumption** for `kimi acp`; if it doesn't exit on EOF, the `kimi-acp` shim kills the child on EOF (rootfs-internal change only).
- **M5 — Android.** Tunnel, QR onboarding (§7: keygen client-side → `/api/v1/enroll` → tunnel import), agent home screen, conversation chat with per-type components (bubbles, thinking cards, tool-call cards), full-screen question dialogs, foreground-service WS + notifications, cancel, reconnect healing. Tests per §9.
- **M6 — Hardening.** TLS/h2 for SSE profile; WS origin checks; ping/pong liveness; `IPAddressAllow` for LLM endpoints; token rotation; per-agent resource caps revisited (uniform unit caps until per-instance properties land); logs/metrics; CI (fmt/clippy/test; Android lint); user guide.

## 9. Testing strategy

- **Rust (default, no root/systemd):** unit (codec, routing, capability policy, bootstrap path/ACL logic, catalog validation) + integration (axum WS/SSE against FakeProvider; tokio UnixStream/duplex pairs; snapshot transcripts; tokio time-driven cancel/reconnect; reflink-vs-copy equivalence).
- **systemd/chroot tests** (optional-gated: root + `systemctl is-system-running` == running, else `#[ignore]` with a printed skip — no provisioning work is scheduled; they run where a normal systemd host/VM happens to exist): real socket-activated spawn of a stub agent through `kimi-acp-enter` — assert bootstrap handshake (ready + each error reason), chroot entry, env; containment asserts from inside (host paths invisible, `/workspace` writable, base image immutable); DynamicUser + ACL cleanup round-trip (gateway deletes a rootfs full of foreign-uid files).
- **E2E:** `scripts/e2e.sh` natively on a systemd host/VM (gateway + real units + fake or real kimi + CLI over WS); WireGuard leg on the host with `wg-quick` (or `wireguard-go` binary) — no containers anywhere. Optional-gated like the above.
- **Android:** JVM model/codec tests, MockWebServer flows, Compose question-sheet tests; manual E2E checklist (emulator → WG → gateway → agent).

## 10. Risks & mitigations

| Risk | Mitigation |
|---|---|
| `kimi acp` may not exit on stdin EOF | Verify in M4; fallback = `kimi-acp` shim kills child on EOF; `TimeoutStopSec=10` backstop. |
| Reflink/hardlink safety (agents must never mutate shared base inodes) | Never hardlink; reflink (COW) or full copy only; M2 test writes through a clone and asserts base unchanged. |
| DynamicUser uid churn vs. file ownership/cleanup | `CAP_DAC_OVERRIDE` at runtime + default ACLs (`group:kimi-gateway:rwx`) + setgid on workspace/state; cleanup round-trip test. |
| chroot alone is a weak boundary | Layered: chroot + minimal ambient caps (SYS_CHROOT/DAC_OVERRIDE only) + `NoNewPrivileges` + syscall filter + `ProtectSystem=strict` pre-chroot + no setuid binaries in base images + random uid. Threat model in operations.md. |
| resolv.conf/DNS staleness inside rootfs | Gateway refreshes identity files at bootstrap and TTL-refresh on connect. |
| Wrapper parsing a hostile bootstrap line | Socket is `0660 kimi-gateway` only; id regex + ready-marker check; wrapper holds caps only pre-chroot and reduces to nothing it can abuse post-exec. |
| systemd socket-activation unavailable in dev | `FakeProvider` covers M1–M3 + unit tests; systemd tests gated, run on the systemd runner. |
| Schema 1.23.0 ⇄ kimi SDK 1.3.0 wire skew | Conformance gate vs real `kimi acp` in M4 (before Android). |
| ACP v1 no stream replay | Reconnect + `session/load` replay (native); gateway-side transparent crash healing (§5.3). |
| RFD is a moving draft and not schema-encoded | Pin schema 1.23.0; implement transport conformance from the vendored RFD text; re-check RFD drift at M6 (h2/cookies already accounted). |
| Gateway restart loses its connection table | Table is rebuilt on client re-attach: a gateway restart detaches all agents (state → `stopped`); clients reconnect their WS and the gateway reconnects to the agent with `initialize` + `session/load`. |
| Enrollment helper is a setuid-root exception to the unprivileged-gateway design | `kimi-enroll-helper` is a separate minimal binary (`apply <peers-file>` only): canonicalizes the path under `/run/kimi-gateway/wg-peers/`, strict peer-stanza validation, absolute `execve` of `/usr/bin/wg` with scrubbed env, no shell. The gateway never holds the privilege. |
| One turn per session (`turn.agent_busy`) | Clients disable send during a turn, same UX as kimi. |
| Base image sprawl / disk usage | Reflinks make clones cheap on supported fs; `kimi-gateway` prune commands; document fs requirements (btrfs/xfs/ext4+reflink or accept full copy). |

## 11. Decisions log (Rev 5 — resolves the Rev 4 open questions; flag to override)

1. **Every agent chrooted, always**; uid separation via per-instance DynamicUser. Stronger isolation (user namespaces, per-agent netns) is future work.
2. **Gateway ↔ systemd: zero runtime coupling** — spawn-by-connect only; no polkit, no D-Bus.
3. **Uniform resource caps** (`MemoryMax=4G`, `TasksMax=256`); per-agent knobs are future work.
4. **Idle agent disconnect** after 10 min of *true* quiescence — any NDJSON line in either direction resets the timer, so in-flight turns are never cut; `idle_timeout=0` keeps the connection indefinitely.
5. **Agent creation open to any authenticated device** (single-admin deployment), constrained by the published catalog.
6. Plain HTTP inside WireGuard; TLS remains an M6 option.
7. **RFD conformance (verified in Rev 5):** the transport RFD is a draft document, not encoded in the schema crate; the vendored schema crate is **1.23.0** (plan previously said 1.9.1 — corrected). The POST+SSE profile requires HTTP/2 (M6); the WebSocket profile is conformant over HTTP/1.1 and is primary. HTTP-profile clients must handle cookies.
8. **Reconnect UX:** a reconnecting client gets its conversation back via `session/load` replay; the abbreviated-history UX of ACP v1 replay is accepted.
9. **Agent crashes are invisible to clients:** the gateway transparently respawns, re-initializes, and re-loads sessions (§5.3).
10. **Bootstrap handshake defined** (§5.2): `{"agent":...}` → `{"status":"ready"}` / `{"status":"error","reason":...}`; mapped to 404/503.
11. **Workspace seeds are client-selectable templates** from `GET /api/v1/config-catalog` (server-local dirs in v1).
12. **One shared kimi login for all agents**, baked into the base image; no per-agent login/credentials in v1.
13. **Reflink is best-effort** — probe `FICLONE`, fall back to full copy; btrfs/xfs/ext4+reflink documented as preferred.
14. **Device tokens expire** (server-defined `token_ttl`); expired → 401; rotation via `add-peer` reissue.
15. **Android background:** foreground service holds the WS; notifications for reverse-RPC questions and turn completion. Android work before M5 is a skeleton placeholder only.
16. **systemd/chroot/E2E tests are optional-gated** — run where a normal systemd host exists, otherwise skip; no VM provisioning scheduled.
17. **Android UX (decided):** conversation-like Material 3 dark chat; per-output-type UI components (bubbles, collapsible thinking cards, tool-call cards, plan checklists); full-screen dialogs for questions/permissions; multiline input; bottom nav (Agents / Sessions / Settings); minimal agent-list badges.
18. **Onboarding is QR provisioning (decided):** `add-peer` prints a link/QR (endpoint, server WG pubkey, gateway IP, assigned client IP, device token); the WireGuard keypair is generated **on the client**; `POST /api/v1/enroll` binds the client pubkey to the token and installs the WG peer via the dedicated setuid binary `kimi-enroll-helper` (§7.1) — the gateway process itself stays fully unprivileged.

## 12. Future (out of scope)

- Per-agent resource/network profiles via extra unit flavors; user-namespace jails (`PrivateUsers=`); per-agent LLM-egress firewalling (`IPAddressAllow`).
- A2A Agent Card exposure; ACP v2 features once stable; `agent-client-protocol` runtime SDK adoption; usage dashboards (`usage_update` passthrough).
- Workspace seeding from git (needs selective egress or gateway-side cloning).
