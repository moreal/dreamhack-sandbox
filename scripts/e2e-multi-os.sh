#!/usr/bin/env bash
# wargame-vm multi-OS end-to-end verification (spec.md §9: "새
# `e2e-multi-os.sh`는 프로필 이름을 받고 미리 빌드된 이미지에서 launch 이후를
# 검증한다").
#
# Scope: *everything after* `wargame profile build`. It takes a profile whose
# image is already published and drives the real `wargame` binary through
# launch -> Ready -> guest checks over SSH -> stop -> resume -> ssh -> delete,
# and finally confirms the profile's own installation VM and image VM are
# still there, untouched. The first OS installation is deliberately NOT here:
# it is a manual UTM-console step recorded separately (spec.md §9: "ISO 설치는
# 별도 수동 단계와 증거로 기록한다"), and this script fails fast, before it
# creates anything, if the image it needs is not ready.
#
# Opt-in: nothing in `cargo test` runs this (spec.md §9: "실VM 검사는 opt-in
# 이다"). It boots a real VM.
#
# Usage:
#   scripts/e2e-multi-os.sh <profile> [challenge-ref]
#
#   scripts/e2e-multi-os.sh kali-arm64                 # fixture:1 by default
#   scripts/e2e-multi-os.sh kali-x86_64 fixture:1
#   WARGAME_PROVIDER=dreamhack scripts/e2e-multi-os.sh kali-arm64 dreamhack:15
#
# Environment:
#   WARGAME_HOME          which wargame home to use. Defaults to the real one
#                         (~/Library/Application Support/wargame-vm) and is
#                         NEVER deleted -- the guest SSH key the profile image
#                         trusts, the build records and the profiles all live
#                         there, so a throwaway home has no built image at all.
#                         Export it when the profiles live somewhere else.
#   WARGAME_PROVIDER      challenge source. Defaults to `fixture`, the
#                         in-process one-challenge provider, so no Dreamhack
#                         account and no download is needed. Set
#                         `WARGAME_PROVIDER=dreamhack` (after `wargame auth
#                         login`) and pass a `dreamhack:<id>` ref to run the
#                         same checks against a real challenge.
#   WARGAME_E2E_NETWORK   guest networking for the launch [default: nat].
#                         `isolated` exercises FR-6's host-only mode.
#   WARGAME_E2E_SSH_WAIT  seconds to wait for guest SSH after a launch or a
#                         resume [default: 300].
#   WARGAME_BIN           an already-built `wargame` to drive, instead of
#                         `cargo build -p wargame-cli`.
#
# Requires (README.md "Prerequisites"): jq, ssh, and cargo unless WARGAME_BIN
# is set -- run this from inside `nix develop`. `utmctl`/`tart` are used
# read-only, for listing, and only to confirm the source VMs survived.
#
# Honesty contract: a check this run cannot perform prints `UNVERIFIED: <why>`
# and is counted; it never passes silently. The final summary lists every one
# of them, so "passed" and "passed without checking X" are never the same
# sentence.
#
# Cleanup contract: it must not leave a session VM behind however it ends. The
# EXIT trap works off a snapshot of the sessions that existed *before* the
# launch, so a failure between "the VM is up" and "we know its id" still takes
# the VM with it, and a session that was already here is never touched. It
# deletes sessions only: the profile's installation VM and image VM are never
# stopped, modified or deleted by this script.

set -euo pipefail

# --- arguments --------------------------------------------------------------

usage() {
  cat >&2 <<'EOF'
usage: scripts/e2e-multi-os.sh <profile> [challenge-ref]

  <profile>        a profile whose image is already built (`wargame profiles`
                   must show its image as "ready")
  [challenge-ref]  provider:id to launch [default: fixture:1]

See the header of this script for the environment variables it reads.
EOF
  exit 2
}

case "${1:-}" in
  -h | --help) usage ;;
esac
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  usage
fi

PROFILE="$1"
CHALLENGE="${2:-fixture:1}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

NETWORK="${WARGAME_E2E_NETWORK:-nat}"
SSH_WAIT="${WARGAME_E2E_SSH_WAIT:-300}"

# --- reporting --------------------------------------------------------------

STAGE_NAME=""
STAGE_T0=0
TIMINGS=()
UNVERIFIED=()

die() {
  echo "error: $*" >&2
  exit 1
}

stage() {
  STAGE_NAME="$1"
  STAGE_T0=$SECONDS
  printf '\n==> %s\n' "$STAGE_NAME"
}

# Closes the current stage and records how long it took.
stage_done() {
  local elapsed=$((SECONDS - STAGE_T0))
  TIMINGS+=("$(printf '%6ss  %s' "$elapsed" "$STAGE_NAME")")
  printf '    done (%ss)\n' "$elapsed"
}

ok() { printf '    + %s\n' "$*"; }

# A check that could not be performed. Never a pass: it is printed here, kept,
# and printed again in the summary.
unverified() {
  printf '    UNVERIFIED: %s\n' "$*"
  UNVERIFIED+=("$*")
}

# --- preflight --------------------------------------------------------------

stage "preflight"

: "${WARGAME_HOME:=$HOME/Library/Application Support/wargame-vm}"
export WARGAME_HOME
export WARGAME_PROVIDER="${WARGAME_PROVIDER:-fixture}"

needed=(jq ssh)
if [ -z "${WARGAME_BIN:-}" ]; then
  needed+=(cargo)
fi
for bin in "${needed[@]}"; do
  command -v "$bin" >/dev/null 2>&1 ||
    die "\`$bin\` is required on PATH (run this inside \`nix develop\`)"
done

if [ -n "${WARGAME_BIN:-}" ]; then
  BIN="$WARGAME_BIN"
  [ -x "$BIN" ] || die "WARGAME_BIN=$BIN is not executable"
else
  cargo build -p wargame-cli
  BIN="$REPO_ROOT/target/debug/wargame"
fi

# `WARGAME_VM=fake` swaps in the in-memory VM backend. That is useful for
# checking this script's own control flow, and useless as evidence: say so
# once here, mark every guest-side check UNVERIFIED, and say it again at the
# end, so a fake run can never be mistaken for a real one.
FAKE_BACKEND=0
if [ "${WARGAME_VM:-}" = "fake" ]; then
  FAKE_BACKEND=1
  cat >&2 <<'EOF'

  !! WARGAME_VM=fake is set. No real VM is involved in this run: the guest
  !! is an in-memory fake, so every guest-side check below is UNVERIFIED.
  !! This exercises the script, not the profile.

EOF
fi

ok "WARGAME_HOME=$WARGAME_HOME"
ok "WARGAME_PROVIDER=$WARGAME_PROVIDER"
ok "wargame: $BIN ($("$BIN" --version))"
stage_done

# --- 1. the image must already be ready --------------------------------------
#
# Fail before anything is created (spec.md §6: "불가 조합·ISO 없음·해시 불일치
# -> VM/세션 생성 전 실패"). `profiles --json` is the contract; the table's
# wording is not.

stage "profile $PROFILE: image readiness"

PROFILE_JSON="$("$BIN" profiles --json | jq -c --arg n "$PROFILE" '.[] | select(.name == $n)')"
if [ -z "$PROFILE_JSON" ]; then
  echo "error: no profile named \"$PROFILE\" in $WARGAME_HOME" >&2
  echo "Try: wargame profiles" >&2
  exit 1
fi

UNSUPPORTED="$(jq -r '.unsupported // empty' <<<"$PROFILE_JSON")"
if [ -n "$UNSUPPORTED" ]; then
  echo "error: profile \"$PROFILE\" does not resolve to a runnable combination:" >&2
  echo "  $UNSUPPORTED" >&2
  exit 1
fi

IMAGE_STATE="$(jq -r '.image_state // "unknown"' <<<"$PROFILE_JSON")"
if [ "$IMAGE_STATE" != "ready" ]; then
  cat >&2 <<EOF
error: profile "$PROFILE" has no image to launch from: its image state is "$IMAGE_STATE",
       and this script only verifies what happens *after* an image is built.

The first OS installation is a manual step and is not automated here:

  wargame profile create $PROFILE --iso <installer.iso>   # make the installation VM
  wargame profile console $PROFILE                        # install the OS by hand
  wargame profile eject $PROFILE                          # take the disc out
  wargame profile build $PROFILE                          # provision + publish the image

then re-run: scripts/e2e-multi-os.sh $PROFILE
EOF
  exit 1
fi

BACKEND="$(jq -r '.resolved.backend' <<<"$PROFILE_JSON")"
MODE="$(jq -r '.resolved.mode' <<<"$PROFILE_JSON")"
GUEST_OS="$(jq -r '.resolved.os' <<<"$PROFILE_JSON")"
GUEST_ARCH="$(jq -r '.resolved.arch' <<<"$PROFILE_JSON")"
ok "image state: ready"
ok "resolved: $BACKEND / $MODE / $GUEST_OS / $GUEST_ARCH"
stage_done

# --- 2. the source VMs, before anything runs ---------------------------------
#
# FR-5/AC-05: "중지·재개·삭제 후에도 원본 이미지와 다른 사용자의 VM은
# 보존된다". Snapshot the profile's own VMs now (id, name and status, exactly
# as the backend lists them) so the same listing can be compared at the end.
# Listing only: nothing here starts, stops or writes to a VM.

RECORD="$WARGAME_HOME/builds/$PROFILE/record.json"
INSTALLER_VM_ID=""
IMAGE_VM_ID=""
SOURCE_BEFORE=""

# One line per VM the backend knows, as `<id>\t<status>\t<name>`. Empty output
# with a zero status means "the backend answered and does not have it".
list_backend_vms() {
  case "$BACKEND" in
    utm)
      local utmctl
      utmctl="$(command -v utmctl || true)"
      if [ -z "$utmctl" ] && [ -x /Applications/UTM.app/Contents/MacOS/utmctl ]; then
        utmctl=/Applications/UTM.app/Contents/MacOS/utmctl
      fi
      [ -n "$utmctl" ] || return 1
      # `utmctl list` prints a header line and then `UUID status name`.
      "$utmctl" list | tail -n +2 |
        sed -E 's/^([^ ]+)[[:space:]]+([^ ]+)[[:space:]]+(.*)$/\1\t\2\t\3/'
      ;;
    tart)
      command -v tart >/dev/null 2>&1 || return 1
      tart list --format json |
        jq -r '.[] | [.Name, (if .Running then "started" else "stopped" end), .Name] | @tsv'
      ;;
    *) return 1 ;;
  esac
}

# The row for one id, or nothing. `$1` is an id from wargame's own records --
# never a string that came out of a guest.
backend_vm_row() {
  local id="$1" listing="$2"
  printf '%s\n' "$listing" | awk -F'\t' -v id="$id" '$1 == id { print }'
}

stage "source VMs before the run"

if [ -f "$RECORD" ]; then
  INSTALLER_VM_ID="$(jq -r '.installer_vm.id // empty' "$RECORD")"
  IMAGE_VM_ID="$(jq -r '.image.id // empty' "$RECORD")"
  ok "build record: $RECORD"
else
  unverified "no build record at $RECORD, so this run cannot name the profile's source VMs (a Tart profile built before build records existed has none)"
fi

if SOURCE_BEFORE="$(list_backend_vms)"; then
  for id in "$INSTALLER_VM_ID" "$IMAGE_VM_ID"; do
    [ -n "$id" ] || continue
    row="$(backend_vm_row "$id" "$SOURCE_BEFORE")"
    if [ -z "$row" ]; then
      die "the build record names $id but $BACKEND does not have it; the profile's image state is stale"
    fi
    ok "source VM present: $row"
  done
  if [ -z "$INSTALLER_VM_ID" ] && [ -z "$IMAGE_VM_ID" ]; then
    unverified "the build record names neither an installation VM nor a published image VM, so there is nothing to compare afterwards"
  fi
else
  SOURCE_BEFORE=""
  unverified "no read-only way to list $BACKEND VMs on this host (utmctl/tart not found), so source preservation cannot be confirmed"
fi
stage_done

# --- 3. cleanup trap ---------------------------------------------------------

session_ids_json() { "$BIN" sessions --json | jq -c '[.[].id]'; }

# Every session id that appeared since we started, newest first.
sessions_we_created() {
  "$BIN" sessions --json |
    jq -r --argjson before "$SESSIONS_BEFORE" \
      '.[] | select(.id as $id | $before | index($id) | not) | .id'
}

SESSIONS_BEFORE="$(session_ids_json)"

cleanup() {
  local ids id
  if ! ids="$(sessions_we_created)"; then
    echo "warning: could not list sessions to clean up; check \`wargame sessions\` yourself" >&2
    return
  fi
  for id in $ids; do
    echo "==> cleaning up $id"
    "$BIN" delete "$id" --force >/dev/null 2>&1 ||
      echo "warning: could not delete $id; run: wargame delete $id --force" >&2
  done
}
trap cleanup EXIT

# --- 4. doctor ---------------------------------------------------------------

stage "wargame doctor --profile $PROFILE"
# Informational, like scripts/e2e-smoke.sh's `doctor`: a failing row here
# (auth against the fixture provider, a disk-space warning against the
# profile's nominal disk size) does not mean the launch cannot work.
"$BIN" doctor --profile "$PROFILE" || echo "    note: doctor exited non-zero (informational)"
stage_done

# --- 5. launch ---------------------------------------------------------------

stage "launch $CHALLENGE --profile $PROFILE --network $NETWORK"
"$BIN" launch "$CHALLENGE" --profile "$PROFILE" --network "$NETWORK" --yes
stage_done

stage "identify the session"
# From `sessions --json`, not from `launch`'s prose: the human output is free
# to change wording, the JSON is the contract.
SESSION_ID="$(sessions_we_created)"
SESSION_ID="${SESSION_ID%%$'\n'*}" # newest first; ignore anything older
[ -n "$SESSION_ID" ] || die "launch created no new session in $WARGAME_HOME"
ok "session: $SESSION_ID"
stage_done

# --- 6. what the session recorded --------------------------------------------

inspect() { "$BIN" session inspect "$SESSION_ID" --json; }

stage "session record"
INSPECT_JSON="$(inspect)"

SESSION_STATE="$(jq -r '.state' <<<"$INSPECT_JSON")"
[ "$SESSION_STATE" = "ready" ] ||
  die "session state is \"$SESSION_STATE\", expected \"ready\" after launch"
ok "state: ready"

# FR-5: the session stores its own backend, mode, guest and connection, and
# every later operation routes on those rather than on the profile.
for pair in \
  "vm_ref.backend|$BACKEND" \
  "vm_mode|$MODE" \
  "guest_os|$GUEST_OS" \
  "guest_arch|$GUEST_ARCH" \
  "network|$NETWORK"; do
  field="${pair%%|*}"
  want="${pair#*|}"
  got="$(jq -r ".$field // \"\"" <<<"$INSPECT_JSON")"
  [ "$got" = "$want" ] || die "session $field is \"$got\", expected \"$want\""
  ok "$field: $got"
done

GUEST_IP="$(jq -r '.guest_ip // empty' <<<"$INSPECT_JSON")"
GUEST_USER="$(jq -r '.connection.user' <<<"$INSPECT_JSON")"
GUEST_PORT="$(jq -r '.connection.port' <<<"$INSPECT_JSON")"
GUEST_SHELL="$(jq -r '.connection.shell' <<<"$INSPECT_JSON")"
GUEST_KEY="$(jq -r '.connection.key_path // empty' <<<"$INSPECT_JSON")"
[ -n "$GUEST_KEY" ] || GUEST_KEY="$WARGAME_HOME/keys/id_ed25519"
WORKSPACE_ROOT="$(jq -r '.workspace_root' <<<"$INSPECT_JSON")"
SLUG="$(jq -r '.challenge.slug' <<<"$INSPECT_JSON")"
SESSION_VM_ID="$(jq -r '.vm_ref.id' <<<"$INSPECT_JSON")"

[ -n "$GUEST_IP" ] || die "session has no guest_ip: $INSPECT_JSON"
[ -n "$WORKSPACE_ROOT" ] || die "session records no workspace_root"
ok "guest: $GUEST_USER@$GUEST_IP:$GUEST_PORT ($GUEST_SHELL), key $GUEST_KEY"
ok "workspace: $WORKSPACE_ROOT (challenge \"$SLUG\")"
stage_done

# --- 7. the guest itself -----------------------------------------------------
#
# Everything below runs one fixed command per SSH round trip. Values that go
# into a remote command are wargame's own records (a workspace root, a
# filename it staged), and each one is single-quoted by `shq` before it is
# interpolated -- the same rule as `wargame_core::infra::ssh::shell_quote`.
# Nothing a guest *prints* is ever put back into a command line, on this host
# or in the guest.

shq() { printf "'%s'" "${1//\'/\'\\\'\'}"; }

guest_ssh() {
  ssh -i "$GUEST_KEY" \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=5 -o LogLevel=ERROR -o BatchMode=yes \
    -p "$GUEST_PORT" "$GUEST_USER@$GUEST_IP" "$1"
}

wait_for_ssh() {
  local deadline=$((SECONDS + SSH_WAIT))
  while :; do
    if guest_ssh true >/dev/null 2>&1; then
      return 0
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      return 1
    fi
    sleep 5
  done
}

# Why this run cannot look inside the guest, or empty if it can.
guest_checks_blocked() {
  if [ "$FAKE_BACKEND" = 1 ]; then
    echo "WARGAME_VM=fake: the guest is an in-memory fake with no SSH server"
    return
  fi
  if [ "$GUEST_SHELL" != "posix" ]; then
    echo "this session's guest shell is \"$GUEST_SHELL\"; the guest-side checks here are POSIX-only (a Windows guest needs the guest/bootstrap.ps1 equivalents, which this script does not implement)"
    return
  fi
  echo ""
}

GUEST_BLOCKED="$(guest_checks_blocked)"

stage "guest over SSH"
if [ -n "$GUEST_BLOCKED" ]; then
  unverified "guest architecture ($GUEST_BLOCKED)"
  unverified "guest workspace tree incoming/work/notes/output ($GUEST_BLOCKED)"
  unverified "guest file hashes vs the host staging hashes ($GUEST_BLOCKED)"
else
  wait_for_ssh || die "guest SSH never answered within ${SSH_WAIT}s at $GUEST_USER@$GUEST_IP:$GUEST_PORT"
  ok "ssh reachable"

  # AC-02/AC-03: the guest really is the architecture the profile claims --
  # which is the whole point of `mode: emulate` on an Apple Silicon host.
  UNAME_M="$(guest_ssh 'uname -m')"
  case "$GUEST_ARCH" in
    arm64) want_uname="aarch64 arm64" ;;
    x86_64) want_uname="x86_64 amd64" ;;
    *) want_uname="" ;;
  esac
  if [ -z "$want_uname" ]; then
    unverified "no expected \`uname -m\` is known for arch \"$GUEST_ARCH\"; the guest reported \"$UNAME_M\""
  elif [[ " $want_uname " == *" $UNAME_M "* ]]; then
    ok "uname -m: $UNAME_M (matches $GUEST_ARCH)"
  else
    die "guest reports \`uname -m\` = \"$UNAME_M\", but the profile resolved to $GUEST_ARCH (expected one of: $want_uname)"
  fi

  # FR-4: "incoming, work, notes, output과 문제 메타데이터를 만든다".
  GUEST_WS="$WORKSPACE_ROOT/$SLUG"
  tree_cmd="for d in incoming work notes output; do [ -d $(shq "$GUEST_WS")/\$d ] || { echo \"\$d\" >&2; exit 1; }; done; echo ok"
  if missing="$(guest_ssh "$tree_cmd" 2>&1 >/dev/null)"; then
    ok "workspace tree: $GUEST_WS/{incoming,work,notes,output}"
  else
    die "guest workspace $GUEST_WS is missing: ${missing:-<unknown>}"
  fi

  # FR-4: "게스트 파일 해시는 호스트 staging 해시와 같아야 한다". The host's
  # side of that comparison is `session inspect --json`'s `assets[]`, which is
  # what it recorded while staging; the guest's side is computed here, now.
  ASSET_COUNT="$(jq -r '.assets | length' <<<"$INSPECT_JSON")"
  if [ "$ASSET_COUNT" -eq 0 ]; then
    unverified "challenge \"$SLUG\" has no staged assets, so there are no hashes to compare"
  else
    sha_probe='if command -v sha256sum >/dev/null 2>&1; then echo sha256sum; elif command -v shasum >/dev/null 2>&1; then echo shasum; elif command -v openssl >/dev/null 2>&1; then echo openssl; else echo none; fi'
    SHA_TOOL="$(guest_ssh "$sha_probe")"
    if [ "$SHA_TOOL" = "none" ]; then
      unverified "no SHA-256 tool (sha256sum/shasum/openssl) in this guest, so the staged files' hashes cannot be recomputed there"
    else
      ok "guest hash tool: $SHA_TOOL"
      while IFS=$'\t' read -r filename want_sha; do
        [ -n "$filename" ] || continue
        remote_path="$GUEST_WS/incoming/$filename"
        case "$SHA_TOOL" in
          sha256sum) hash_cmd="sha256sum -- $(shq "$remote_path")" ;;
          shasum) hash_cmd="shasum -a 256 -- $(shq "$remote_path")" ;;
          openssl) hash_cmd="openssl dgst -sha256 $(shq "$remote_path")" ;;
        esac
        got_sha="$(guest_ssh "$hash_cmd" | tr -d '\r' | grep -Eo '[0-9a-f]{64}' | head -n1 || true)"
        if [ -z "$got_sha" ]; then
          die "could not hash $remote_path in the guest (is the staged file there?)"
        fi
        [ "$got_sha" = "$want_sha" ] ||
          die "hash mismatch for $filename: host staged $want_sha, guest computed $got_sha"
        ok "hash matches host staging: $filename ($want_sha)"
      done < <(jq -r '.assets[] | [.filename, .sha256] | @tsv' <<<"$INSPECT_JSON")
    fi
  fi
fi
stage_done

# --- 8. stop -> resume -> ssh ------------------------------------------------
#
# FR-5: a stopped session comes back with the settings it stored, on the
# backend it stored, whatever the profile says now.

stage "stop $SESSION_ID"
"$BIN" stop "$SESSION_ID"
STATE_AFTER_STOP="$(inspect | jq -r '.state')"
[ "$STATE_AFTER_STOP" = "stopped" ] ||
  die "session state after stop is \"$STATE_AFTER_STOP\", expected \"stopped\""
ok "state: stopped"
stage_done

stage "resume $SESSION_ID"
"$BIN" resume "$SESSION_ID"
INSPECT_JSON="$(inspect)"
STATE_AFTER_RESUME="$(jq -r '.state' <<<"$INSPECT_JSON")"
case "$STATE_AFTER_RESUME" in
  ready | running) ok "state: $STATE_AFTER_RESUME" ;;
  *) die "session state after resume is \"$STATE_AFTER_RESUME\", expected ready or running" ;;
esac
# A resumed VM can come back on a different address.
NEW_IP="$(jq -r '.guest_ip // empty' <<<"$INSPECT_JSON")"
[ -n "$NEW_IP" ] || die "resumed session has no guest_ip"
if [ "$NEW_IP" != "$GUEST_IP" ]; then
  ok "guest ip changed on resume: $GUEST_IP -> $NEW_IP"
  GUEST_IP="$NEW_IP"
fi
stage_done

stage "ssh after resume"
if [ -n "$GUEST_BLOCKED" ]; then
  unverified "guest reachable after resume ($GUEST_BLOCKED)"
else
  wait_for_ssh || die "guest SSH did not come back within ${SSH_WAIT}s after resume"
  RESUMED_UNAME="$(guest_ssh 'uname -m')"
  ok "guest answered after resume: uname -m = $RESUMED_UNAME"
fi
stage_done

# --- 9. delete ---------------------------------------------------------------

stage "delete $SESSION_ID"
"$BIN" delete "$SESSION_ID" --force
if "$BIN" sessions --json | jq -e --arg id "$SESSION_ID" 'any(.[]; .id == $id)' >/dev/null; then
  die "session $SESSION_ID is still listed after delete"
fi
ok "session gone from \`wargame sessions\`"

if [ -n "$SOURCE_BEFORE" ]; then
  SOURCE_AFTER="$(list_backend_vms || true)"
  if [ -n "$(backend_vm_row "$SESSION_VM_ID" "$SOURCE_AFTER")" ]; then
    die "the session's VM $SESSION_VM_ID is still listed by $BACKEND after delete"
  fi
  ok "session VM gone from $BACKEND: $SESSION_VM_ID"
else
  SOURCE_AFTER=""
  unverified "the session's VM could not be confirmed gone from $BACKEND (no listing tool)"
fi
stage_done

# --- 10. the source VMs survived ---------------------------------------------

stage "source VMs after the run"
if [ -n "$SOURCE_BEFORE" ] && [ -n "$SOURCE_AFTER" ]; then
  checked=0
  for id in "$INSTALLER_VM_ID" "$IMAGE_VM_ID"; do
    [ -n "$id" ] || continue
    before="$(backend_vm_row "$id" "$SOURCE_BEFORE")"
    after="$(backend_vm_row "$id" "$SOURCE_AFTER")"
    [ -n "$after" ] || die "source VM $id is gone after the run: it was \"$before\""
    [ "$before" = "$after" ] ||
      die "source VM $id changed during the run: \"$before\" -> \"$after\""
    ok "unchanged: $after"
    checked=$((checked + 1))
  done
  if [ "$checked" -eq 0 ]; then
    unverified "no source VM id was known, so preservation was not checked"
  elif [ -z "$INSTALLER_VM_ID" ]; then
    unverified "this profile's build record names no installation VM (Tart profiles have none), so only the published image was checked"
  fi
else
  unverified "source VMs could not be listed after the run, so preservation was not confirmed"
fi
stage_done

# --- summary -----------------------------------------------------------------

printf '\n==> timings\n'
for line in "${TIMINGS[@]}"; do
  printf '%s\n' "$line"
done
printf '%6ss  total\n' "$SECONDS"

printf '\n'
if [ "${#UNVERIFIED[@]}" -eq 0 ]; then
  echo "multi-os e2e passed for profile \"$PROFILE\" ($BACKEND/$MODE, $GUEST_OS/$GUEST_ARCH): nothing was left unverified"
else
  echo "multi-os e2e passed for profile \"$PROFILE\" ($BACKEND/$MODE, $GUEST_OS/$GUEST_ARCH), with ${#UNVERIFIED[@]} check(s) UNVERIFIED:"
  for line in "${UNVERIFIED[@]}"; do
    printf '  - %s\n' "$line"
  done
  if [ "$FAKE_BACKEND" = 1 ]; then
    echo
    echo "This was a WARGAME_VM=fake run. It proves this script's control flow and"
    echo "the CLI's session bookkeeping -- it is not evidence about any guest."
  fi
fi
