# Papin Android client

Kotlin / Jetpack Compose client for the remote Papin agent harness (PLAN §7).
Conversation-like Material 3 dark chat: agent list home screen, streaming chat
with per-output-type components (bubbles, collapsible thinking cards,
tool-call cards, plan checklists), full-screen question dialogs, QR
onboarding with device-side WireGuard key generation, and a foreground
service that holds the ACP WebSocket while backgrounded.

## Status: source-complete; non-UI logic verified, SDK build verification pending

This tree was authored on a machine **without an Android SDK / gradle**. The
Android-dependent code (Compose UI, services, manifest) has **not been
compiled** — first build on an SDK machine per below. Everything non-UI
(ACP envelope codec, chat reducer, provisioning-link parser, enroll flow
state machine, GatewayApi, AcpClient) is plain Kotlin/JVM and **was compiled
and executed here with a standalone Kotlin compiler**: all 27 JVM unit tests
pass (codec, provisioning, reducer, enroll flow, MockWebServer REST + WS).

First build on a machine with the SDK (API 34 platform + JDK 17):

```sh
cd android
gradle wrapper --gradle-version 8.9   # one-time: generates gradle/wrapper/gradle-wrapper.jar
./gradlew build                       # compile + unit tests
./gradlew connectedDebugAndroidTest   # on an emulator/device (optional)
```

`gradlew`/`gradlew.bat` and `gradle-wrapper.properties` are committed; the
wrapper **jar** is a binary artifact this repo cannot author, hence the
one-time `gradle wrapper` step above (`gradlew` prints the same
instructions if the jar is missing).

## Layout

- `core/model/` — pure Kotlin: ACP v1 envelope codec (exact wire field
  names), gateway DTOs, provisioning-link parser, chat reducer, enroll flow.
- `core/acp/` — `AcpClient`: okhttp WebSocket with JSON-RPC id demux;
  notifications / reverse-RPC / untracked responses as a Flow.
- `core/net/` — `GatewayApi`: `/api/v1/agents`, `/config-catalog`, `/enroll`,
  `DELETE`, `/healthz`.
- `core/wg/` — `TunnelManager` on `com.wireguard.android:tunnel`
  (GoBackend, userspace, no root) + `service/PapinVpnService`.
- `core/prefs/` — settings store (gateway URL, device token, tunnel state).
- `service/` — `AcpService` foreground service (dataSync) holding the WS;
  question + turn-completion notifications; tap returns to the chat.
- `ui/` — Compose screens: onboarding (QR via zxing), agents (pull-refresh,
  catalog-driven create dialog), chat, sessions, settings; bottom nav
  Agents / Sessions / Settings.
- `di/` — manual DI (`AppContainer`), no Hilt.

## Onboarding flow (§7.1)

1. Scan the provisioning QR (`rust/scripts/add-peer` output) or open a
   `papin://enroll?...` link. The payload carries the WG endpoint, server
   pubkey, gateway IP/port, assigned client IP, and the device token —
   **never a private key**.
2. The app generates its WireGuard keypair on the device.
3. `POST /api/v1/enroll {client_pubkey}` (bearer = device token) →
   `{assigned_ip, gateway_ip}`.
4. The wg-quick config is built (private key stays on-device), imported via
   the tunnel lib, and the tunnel comes up. Settings persist; the agent
   list is the home screen.

## Offline verification (performed)

The pure-Kotlin modules and their tests are Android-free. They were compiled
with kotlinc 2.0.20 (serialization plugin) and run under JUnit 4 against
Maven jars: **27/27 green**. The same sources compile unchanged in the
`test/` source set when built with the Android plugin.

## Known integration points to check on first real build

- WireGuard tunnel lib: `GoBackend(VpnService)` and the `Tunnel` interface
  (`getName`/`getConfig`/`onStateChange`) — pinned
  `com.wireguard.android:tunnel:1.0.20260315`; adjust if the artifact's API
  surface drifted.
- zxing-android-embedded `ScanContract` result handling.
- Compose Material3 experimental APIs opted into where required
  (`ExperimentalMaterial3Api`): `TopAppBar`, `PullToRefreshBox`,
  `ExposedDropdownMenuBox`.
