# Operations — Remote Papin Agent Harness

Install, agents, base images, WireGuard peers, kimi login, troubleshooting,
and the threat model. Companion to [PLAN.md](PLAN.md) and
[architecture.md](architecture.md). M4 deliverable.

## 1. Install (Debian packages)

Two binary packages are built from this tree with `debian/` (see
`debian/README` for build details, the `CARGO_TARGET_DIR` caveat, and
offline/vendoring notes):

```sh
(cd rust && dpkg-buildpackage -us -uc -b)   # produces ../papin-cli_*.deb, ../papin-gateway_*.deb
sudo dpkg -i papin-cli_*.deb papin-gateway_*.deb
```

(Or `make -C rust deb` / `make deb` from the repo root.)

- **`papin-cli`** — the terminal client (`/usr/bin/papin`). No systemd, no root.
- **`papin-gateway`** — the server: gateway daemon and chroot wrapper under
  `/usr/lib/papin-gateway/`, the systemd units, ops tooling, docs, and the
  setuid enroll helper at `/usr/local/lib/papin-gateway/papin-enroll-helper`
  (root:root **4755** — the plan's single root-capable component, the only
  file outside dpkg's normal `/usr` ownership). On install it creates the
  `papin-gateway` system user, the state tree, and the tokens file, then
  enables and starts `papin-gateway.service` and `papin-agent.socket`.

`gateway.toml` is a **conffile** (`/etc/papin-gateway/`); the mutable device
tokens live at `/var/lib/papin-gateway/tokens.toml` (deliberately *not* a
conffile — runtime data owned root:papin-gateway 0640). Removal:

```sh
sudo apt purge papin-gateway papin-cli   # purge also drops /var/lib/papin-gateway and the user
```

Layout after install:## 2. Filesystem guidance (bootstrap performance)

Agent rootfs trees are **clones of a catalog base image**: FICLONE reflink
where the filesystem supports it (copy-on-write, cheap), silent full-copy
fallback otherwise. **Never hardlink** — agents must not mutate shared
inodes. Preferred filesystems for `/var/lib/papin-gateway`:

- **btrfs** or **xfs** — reflink clones, near-instant agent creation.
- **ext4 with reflink support** (modern kernels) — same.
- **plain ext4** — works at full-copy cost.

ACLs (`setfacl` default ACL `group:papin-gateway:rwx` on `/workspace` and
`/home/papin`) are best-effort like reflink: applied when the tool and fs
support them, skipped silently otherwise. They exist so the gateway (as
`papin-gateway`) can clean up files created by ephemeral DynamicUser uids
(`CAP_DAC_OVERRIDE` in the agent unit is the runtime backstop).

## 3. Base images (`papin-gateway mkbase`) and kimi login

A base image = minimal OS tree + node runtime + kimi install + CA certs.
**No setuid binaries, no package manager, no systemd inside.** Build one
from a prepared directory:

```sh
sudo mkdir -p /var/lib/papin-gateway/rootfs/base-src
# ... populate the OS tree (debootstrap-style or copied from a prepared
#     container/VM image), install node + `npm i -g @kimi/...`, CA certs,
#     and the rootfs-internal shim as /usr/local/bin/papin-acp
sudo ./rust/target/release/papin-gateway --config /etc/papin-gateway/gateway.toml \
    mkbase node22-kimi-1.0 /var/lib/papin-gateway/rootfs/base-src
# publish: add [[catalog.bases]] name = "node22-kimi-1.0" to gateway.toml
systemctl restart papin-gateway.service
```

**One shared kimi login for all agents** (PLAN §5.4): run `kimi login`
**once, at image-build time**, inside the base source tree. The credentials
live in the base image (`/home/papin/.kimi-code`) and are copied (or
reflinked) into every agent rootfs at bootstrap. There is no per-agent
login flow and no per-agent credential store in v1.

The rootfs-internal `/usr/local/bin/papin-acp` shim (in `scripts/papin-acp`)
execs `kimi acp` and enforces the **stdin-EOF-exit assumption** (PLAN §10):
if `kimi acp` does not exit on its own shortly after stdin EOF, the shim
kills it (SIGTERM, then SIGKILL), so the systemd unit's `Restart=no`
EOF-is-clean-exit semantics hold. **Verify on a real host** that `kimi acp`
actually exits on EOF: `printf '' | kimi acp; echo $?` — if it exits
cleanly the shim's kill path never fires.

## 4. Agents

```sh
# From any enrolled device (CLI):
papin agent new my-agent --base node22-kimi-1.0 --seed demo   # catalog-driven
papin --agent my-agent                                        # connect
papin agent rm my-agent [--force]                             # 409 while attached
```

`POST /api/v1/agents` validates strictly against the catalog (base must be
published, seed must exist under `workspaces/`, env keys allow-listed),
bootstraps the rootfs (clone → writable dirs → ACLs → identity files →
workspace seed → `.papin-rootfs-ready` last), and returns immediately — **no
process starts** until the first client attaches. Spawning = connecting:
the gateway's first `connect()` to the socket makes systemd launch a fresh
`papin-agent@<n>` instance (DynamicUser, chrooted, capped). Crash → EOF →
the instance exits; next attach respawns. The gateway transparently
re-initializes and `session/load`s on agent death — clients see at most a
pause.

Agent env tweaks: `/var/lib/papin-gateway/agents/<id>.env` (`KEY=VALUE`,
read by the wrapper pre-chroot and merged into the agent's minimal
environ).

## 5. WireGuard peers + device enrollment (§7.1)

The WireGuard interface itself is managed by the admin with the standard
`wg-quick@wg0.service` — **the gateway holds no `CAP_NET_ADMIN`**. The only
root-capable component is the setuid `papin-enroll-helper`.

```sh
# Server: provision a device (mints token + pool IP into tokens.toml,
# prints a provisioning link + QR — no private key anywhere):
sudo add-peer --name "alice-phone"   # shipped in the papin-gateway package (/usr/sbin)

# Client (app or CLI): scan the QR / open the link. The WireGuard keypair is
# generated ON THE DEVICE. The app calls:
#   POST /api/v1/enroll  {client_pubkey}
#   Authorization: Bearer <token from the link>
# The gateway renders a peers file, runs papin-enroll-helper apply (wg
# syncconf wg0), and returns {assigned_ip, gateway_ip}. Re-running enroll
# with a new key (re-keying) overwrites the binding, keeping the IP.

# WireGuard client config (app-side): private key stays on the device;
# server public key + endpoint + assigned IP come from the QR payload.
```

`add-peer` never touches `wg0` — the peer is installed on first
enrollment. Token lifetime is server-defined (`token_ttl_secs` in
`gateway.toml`); expired/unknown tokens get a 401 with an actionable
message; rotation = re-run `add-peer` (prints a new token; the old entry
expires naturally).

## 6. Troubleshooting

| Symptom | Likely cause / fix |
|---|---|
| `401 unauthorized` | Token unknown/expired → re-run `add-peer`; check device clock. |
| `unknown_agent` (404) | Agent record missing → `papin agent new`; or rootfs/registry removed under a live id. |
| `rootfs_not_ready` (503) | Bootstrap didn't finish (marker missing) → check gateway logs; re-`POST /api/v1/agents` after fixing; delete + recreate the agent if wedged. |
| `exec_failed` (503) | `papin-acp` missing inside the rootfs (base image without the shim) or node broken → rebuild the base image. |
| Agent `active` but silent | Look at the instance journal: `journalctl -u 'papin-agent@*' -f`. Crash → gateway respawns on next activity. |
| `wg0` down | `wg-quick up wg0`; check `wg show`. `papin doctor` from a client runs the whole checklist. |
| Peers not connecting after enroll | `papin-enroll-helper apply` runs `wg syncconf wg0 <file>` — its stderr is returned by `POST /api/v1/enroll` and logged; check `wg show wg0`. |
| Enrollment 500 `wg_syncconf` | Helper rejected the peers file (should not happen — gateway renders it) or `wg` failed; run `papin-enroll-helper apply <file>` manually as root to see why. |
| Disk filling | Agent rootfs trees are reflink clones (cheap) on btrfs/xfs/ext4+reflink; on plain ext4 each agent is a full copy — prune with `papin agent rm`. |

Logs: gateway → `journalctl -u papin-gateway`; agents → `journalctl -u
'papin-agent@*'` (each instance is one connection; ids are anonymous
`<n>`).

## 7. Threat model + audit notes (papin-enroll-helper)

Design (PLAN §7.1, §10): **the gateway process is fully unprivileged** —
spawning agents is a `connect(2)` to a socket, and installing WireGuard
peers is a subprocess call. Exactly one component in the system executes
with root privilege: `papin-enroll-helper`, installed setuid-root, mode
4755. Everything else runs as `papin-gateway` (service) or per-instance
`DynamicUser` (agents).

Layering, outside → in:

1. **Socket + bearer token.** The agent socket is `0660 root:papin-gateway`;
   every HTTP route except `/healthz` requires `Authorization: Bearer
   <device-token>` (provisioned by `add-peer`, expiring). The enrollment
   endpoint additionally validates the token→entry binding.
2. **Minimal interface.** The helper accepts exactly `apply <peers-file>`
   — no other subcommands, no flags, no config, no network. Anything else
   exits 2 with a usage error.
3. **Path validation (defense in depth).** The peers file must canonicalize
   to a regular file inside `/run/papin-gateway/wg-peers/`; symlinks are
   rejected (via `symlink_metadata`), escapes rejected (canonicalization),
   non-files rejected. The directory is `root:papin-gateway 0770`, so only
   the gateway can stage files the helper will accept.
4. **Content validation (defense in depth).** Strict `wg-quick`-style
   subset: comments, blanks, `[Peer]` stanzas with `PublicKey = <base64>`
   (44-char standard-padded WireGuard key) and `AllowedIPs = <CIDR>[,…]`
   (v4/v6, prefix bounds). Any other key (`Endpoint`, `PersistentKeepalive`,
   `[Interface]`, …) is rejected. This makes the file a pure *peer* file:
   it cannot alter `wg0`'s private key, listen port, or addresses.
5. **Constrained execution.** `fork`+`execve` of the absolute path
   `/usr/bin/wg` with arguments `syncconf wg0 <file>` and a scrubbed fixed
   environment (`env_clear` + `PATH` + `LC_ALL`) — no shell anywhere, no
   PATH lookup ambiguity, no inherited environment.
6. **Capability posture.** The helper needs no ambient capabilities beyond
   the setuid root it already has; `wg syncconf` needs root only for the
   netlink `wg0` configuration. The gateway never holds `CAP_NET_ADMIN`.
   Agents run with `CapabilityBoundingSet=CAP_SYS_CHROOT
   CAP_DAC_OVERRIDE` + `NoNewPrivileges=yes`, chrooted, on a random uid —
   they cannot see host paths or affect the host.

Audit notes:

- All validation logic lives in `papin_enroll_helper::lib` and is unit
  tested (args, path escapes/symlinks, stanza grammar, pubkey/CIDR forms);
  `main` is a thin shell that cannot reach `wg` without passing it.
- The gateway renders the peers file itself and it must pass the helper's
  own validator — an accidental grammar drift fails closed (enroll returns
  the helper's stderr).
- The setuid surface is one binary, one subcommand, ~100 lines of `main`;
  review that plus the validators and you have reviewed the entire
  privilege boundary.
- Residual risks: `wg` itself is trusted (as any setuid system's target
  binary must be); a compromised gateway could stage *valid* peer files
  (that's by design — it is the enrollment authority) but cannot run
  arbitrary code as root, read arbitrary files, or reconfigure `wg0`
  beyond adding/replacing peers.

## 7.1 Android onboarding

The Android app (`android/`, see its README) provisions via QR or a
`papin://enroll?...` link printed by `add-peer`: it generates its WireGuard
keypair on-device, calls `POST /api/v1/enroll`, imports the returned tunnel
config (userspace WireGuard via `com.wireguard.android:tunnel`), and shows
the agent list. The QR never carries a private key; re-keying = scan a fresh
`add-peer` link (enrollment overwrites the token's pubkey binding, keeping
the assigned IP).

## 8. Testing

- Default `cargo test` runs everything **without root/systemd**: codec and
  routing units, WS/POST integration vs FakeProvider, registry REST,
  session surface, bootstrap (copy fallback + ACL-skip paths), reflink/copy
  equivalence and base-image immutability, enrollment against a stub
  helper, the wrapper's error handshakes (binary level), enroll-helper
  validators.
- Optional-gated (PLAN §9), run with `cargo test -- --ignored` on a normal
  systemd host/VM as root: `enter_chroots_and_execs_stub_agent` (real
  chroot + static in-chroot ACP stub), `systemd_units_verify`
  (systemd-analyze on the shipped units). Both print a skip reason when a
  prerequisite is missing and never require privileges in the default run.
- Native E2E (systemd host, after `dpkg -i` of the packages):
  `sudo rust/scripts/e2e.sh` (also shipped at
  `/usr/share/papin-gateway/e2e.sh`) — provisions a token, restarts the
  gateway, and runs a full CLI-over-WS turn against it. Skips with a
  printed reason when the units are not installed or systemd is not
  running.
