#!/bin/bash
# Native end-to-end smoke test (PLAN §9): gateway + installed units + fake or
# real agent + CLI over WS. No containers. Requires a normal systemd host.
#
# Optional-gated: skips with a printed reason unless run as root on a system
# where `systemctl is-system-running` reports "running".
set -euo pipefail

skip() { echo "SKIP: $1"; exit 0; }

[[ $EUID -eq 0 ]] || skip "e2e.sh needs root (installs units, starts services)"
systemctl is-system-running 2>/dev/null | grep -qx running \
    || skip "systemd is not fully running (got: $(systemctl is-system-running 2>&1 || true)); run on a normal systemd host/VM"

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FAKE="${FAKE:-1}"   # FAKE=0 uses the real `kimi acp` (requires a built base image + login)
TOKEN="e2e-token-$$"
AGENT="e2e-agent"
command -v systemctl >/dev/null || skip "no systemctl"
ls /usr/lib/systemd/system/papin-gateway.service /etc/systemd/system/papin-gateway.service >/dev/null 2>&1 \
    || skip "papin-gateway units not installed (install the papin-gateway package first)"

echo "==> ensuring a CLI is available"
CLI=/usr/bin/papin-cli
if [[ ! -x $CLI ]]; then
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/rust/target}"
    cargo build -p papin-cli --manifest-path "$REPO/rust/Cargo.toml"
    CLI="${CARGO_TARGET_DIR}/debug/papin-cli"
fi

echo "==> provisioning a device token"
NOW="$(date +%s)"
cat >> /var/lib/papin/tokens.toml <<EOF

[[tokens]]
token = "$TOKEN"
created_at = $NOW
expires_at = $((NOW + 3600))
EOF

if [[ $FAKE == 1 ]]; then
    echo "==> e2e with the fake agent (scripts/fake-papin-acp)"
    sed -i 's|^provider = .*|provider = "fake"|; s|^fake_script = .*|fake_script = "'"$REPO"'/rust/scripts/fake-papin-acp"|' \
        /etc/papin/gateway.toml
    grep -q fake_script /etc/papin/gateway.toml || \
        sed -i 's|^\[catalog\]|fake_script = "'"$REPO"'/rust/scripts/fake-papin-acp"\n\n[catalog]|' /etc/papin/gateway.toml
fi

echo "==> (re)starting gateway"
systemctl restart papin-gateway.service

echo "==> creating agent + running CLI headless turn over WS"
HTTP="http://$(sed -n -E 's/^listen = "([^"]*)".*/\1/p' /etc/papin/gateway.toml)"
curl -fsS -X POST "$HTTP/api/v1/agents" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"id\": \"$AGENT\", \"config\": {\"base\": \"fake\"}}" >/dev/null || true

OUT="$("$CLI" --config <(printf 'url = "%s"\ntoken = "%s"\n' "$HTTP" "$TOKEN") \
    --agent "$AGENT" -p 'e2e hello')"
echo "CLI output: $OUT"
grep -q "Fake response to: e2e hello" <<<"$OUT" || { echo "FAIL: unexpected CLI output" >&2; exit 1; }

echo "PASS: e2e smoke (gateway + units + agent + CLI over WS)"
