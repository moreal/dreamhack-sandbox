#!/usr/bin/env bash
# wargame-vm end-to-end smoke test (PLAN.md Task 9, MVP.md §35/§36 M0 proof).
#
# Exercises the *whole* CLI -- build, doctor, launch, inspect, guest
# verification over real SSH, delete -- against a real Tart VM, but with
# zero Dreamhack dependency: `WARGAME_PROVIDER=fixture` swaps in the
# in-process `FixtureProvider` (one harmless challenge, `fixture:1`, whose
# asset is a hand-built ZIP containing `hello.txt`), so this needs no
# Dreamhack account and downloads nothing from the network.
#
# It DOES need a real Tart backend: this is not `WARGAME_VM=fake`. The
# named profile's local Tart image must already exist (`wargame profile
# build <profile>`), since building one from scratch takes many minutes.
#
# Usage:
#   scripts/e2e-smoke.sh [profile-name]     # default profile: macos-minimal-arm64
#
# Networking: this uses NAT by default (see README.md "What \"isolated\"
# networking means"), because isolated mode needs a one-time Softnet sudo
# rule that most machines won't have configured; set
# WARGAME_SMOKE_NETWORK=isolated to exercise that path once it is.
#
# Requires (see README.md "Prerequisites"): tart, ssh, jq, cargo -- run this
# from inside `nix develop` so all of them are on PATH.

set -euo pipefail

PROFILE="${1:-macos-minimal-arm64}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

for bin in cargo tart ssh jq; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "error: \`$bin\` is required on PATH (run this inside \`nix develop\`)" >&2
    exit 1
  fi
done

# Deliberately NOT a temporary WARGAME_HOME: the guest SSH key the profile
# image already trusts (installed during `wargame profile build`) lives in
# the real one, so a fresh/temporary home would have no key the image
# accepts. Use whatever the rest of `wargame` uses, and never delete it.
: "${WARGAME_HOME:=$HOME/Library/Application Support/wargame-vm}"
export WARGAME_HOME
export WARGAME_PROVIDER=fixture

SESSION_ID=""
cleanup() {
  if [ -n "$SESSION_ID" ]; then
    "$BIN" delete "$SESSION_ID" --force >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

echo "==> building wargame"
cargo build -p wargame-cli
BIN="$REPO_ROOT/target/debug/wargame"

echo "==> WARGAME_HOME=$WARGAME_HOME"
echo "==> wargame doctor"
"$BIN" doctor || true # informational: some checks (e.g. profile image) are
                       # expected to fail on a machine that hasn't built
                       # $PROFILE yet; that's what the next step is for.

NETWORK="${WARGAME_SMOKE_NETWORK:-nat}"
echo "==> wargame launch fixture:1 --profile $PROFILE --network $NETWORK --yes"
LAUNCH_OUT="$("$BIN" launch fixture:1 --profile "$PROFILE" --network "$NETWORK" --yes)"
echo "$LAUNCH_OUT"

SESSION_ID="$(echo "$LAUNCH_OUT" | sed -n 's/^session id: //p')"
if [ -z "$SESSION_ID" ]; then
  echo "error: could not find a session id in launch output" >&2
  exit 1
fi
echo "==> session: $SESSION_ID"

echo "==> wargame session inspect $SESSION_ID --json"
INSPECT_JSON="$("$BIN" session inspect "$SESSION_ID" --json)"
GUEST_IP="$(echo "$INSPECT_JSON" | jq -r '.guest_ip // empty')"
if [ -z "$GUEST_IP" ]; then
  echo "error: session has no guest_ip" >&2
  echo "$INSPECT_JSON" >&2
  exit 1
fi
echo "==> guest ip: $GUEST_IP"

echo "==> verifying the guest-extracted file over SSH"
GUEST_KEY="$WARGAME_HOME/keys/id_ed25519"
CONTENT="$(ssh -i "$GUEST_KEY" \
  -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
  -o ConnectTimeout=5 -o LogLevel=ERROR -o BatchMode=yes \
  "admin@$GUEST_IP" \
  'cat ~/wargame/hello-fixture/incoming/extracted/hello-fixture/hello.txt')"

if [ "$CONTENT" != "hello wargame" ]; then
  echo "error: unexpected guest file content: $CONTENT" >&2
  exit 1
fi
echo "==> guest file content verified: \"$CONTENT\""

echo "==> wargame delete $SESSION_ID --force"
"$BIN" delete "$SESSION_ID" --force
SESSION_ID="" # already deleted; skip the cleanup trap's delete

echo "==> smoke test passed"
