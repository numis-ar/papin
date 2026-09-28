# Papin — Remote Agent Harness

Papin is a harness for running AI agents as **contained, per-agent processes** behind
a small gateway: clients (Android app, terminal CLI) talk ACP v1 over a
WireGuard tunnel; the gateway spawns each agent in its own chroot with a
fresh dynamic user, proxies and policy-filters the protocol, and heals
crashes transparently.

## Why "Papin"?

Denis Papin (French physicist, 1647–c. 1713) invented the **steam digester**
in 1679 — a sealed pressure vessel in which dangerous reactions could be
carried out and studied without harming the operator. It is the historical
origin of the "containment chamber for safe operation" idea, a direct
ancestor of the steam engine, and its signature safety valve (Papin's own
addition) is a fitting metaphor for this project's permission layer: agents
do risky work *inside a sealed vessel*, and nothing escalates privileges
without an explicit release.

## What's here

| Path | What |
|---|---|
| `rust/` | The whole backend+CLI workspace: `papin-gateway` (ACP façade, registry, bootstrap engine), `papin-acp-enter` (chroot wrapper), `papin-enroll-helper` (setuid WireGuard enrollment), `papin-cli` (ratatui terminal client), `acp-wire` (shared ACP v1 wire library) — plus `systemd/` units, `scripts/` tooling, `debian/` packaging, and a `Makefile`. |
| `android/` | Kotlin/Jetpack Compose client (QR onboarding, chat, foreground-service WS). |
| `docs/` | PLAN (spec), architecture, protocol notes, operations. |
| `resources/` | Vendored upstream references (ACP schema + RFD, kimi-code, WireGuard) — read-only. |

## Build & test

```sh
make -C rust          # release build
make -C rust test     # fmt + clippy + tests
make -C rust deb      # papin-cli + papin-gateway .debs (needs debhelper)
make rust-test / deb  # from the repo root
```

See `docs/operations.md` for install (Debian packages), agents, base
images, WireGuard peers, and troubleshooting.
