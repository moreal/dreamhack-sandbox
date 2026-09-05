#!/usr/bin/env bash
# wargame-vm guest bootstrap (MVP.md §15, DESIGN.md §6/§3.4).
#
# Runs *inside* the disposable guest, never on the host. Responsibilities:
#   1. Create the challenge workspace layout (incoming/work/notes/output).
#   2. Optionally unpack known archive types already staged under incoming/
#      -- guest-side only, and never anything the host doesn't already know
#      is one of a small allow-listed set of archive formats.
#   3. Summarize incoming/.wargame.json (if the host wrote one) into a
#      human-readable notes/README.md.
#   4. Print a short welcome banner.
#
# It NEVER executes anything found inside incoming/ -- extraction only.
#
# Usage: bootstrap.sh <workspace-dir> --extract=<true|false>

set -euo pipefail

usage() {
  echo "usage: $(basename "$0") <workspace-dir> --extract=<true|false>" >&2
  exit 2
}

if [ "$#" -lt 2 ]; then
  usage
fi

WORKSPACE="$1"
shift

EXTRACT=""
for arg in "$@"; do
  case "$arg" in
    --extract=true) EXTRACT="true" ;;
    --extract=false) EXTRACT="false" ;;
    *)
      echo "unrecognized argument: $arg" >&2
      usage
      ;;
  esac
done
if [ -z "$EXTRACT" ]; then
  usage
fi

INCOMING="$WORKSPACE/incoming"
WORK="$WORKSPACE/work"
NOTES="$WORKSPACE/notes"
OUTPUT="$WORKSPACE/output"

mkdir -p "$INCOMING" "$WORK" "$NOTES" "$OUTPUT"

# --- 2. optional guest-side extraction -------------------------------------
#
# `unzip`/`tar` (bsdtar on macOS, GNU tar elsewhere) both refuse by default to
# write a zip-slip entry (an absolute path, or one containing "..") outside
# the destination directory they were given; we additionally scope every
# archive's extraction to its own `incoming/extracted/<name>/` subdirectory
# so even a tolerant implementation cannot escape past `incoming/`.
if [ "$EXTRACT" = "true" ]; then
  shopt -s nullglob

  for archive in "$INCOMING"/*.zip; do
    [ -f "$archive" ] || continue
    name="$(basename "$archive" .zip)"
    dest="$INCOMING/extracted/$name"
    mkdir -p "$dest"
    if ! unzip -n -q "$archive" -d "$dest"; then
      status=$?
      # unzip exits 1 for "one or more warnings" (e.g. a skipped unsafe
      # entry) -- that is the protection working, not a failure. Anything
      # higher is a real error.
      if [ "$status" -gt 1 ]; then
        echo "failed to extract $archive (exit $status)" >&2
        exit "$status"
      fi
    fi
  done

  for archive in "$INCOMING"/*.tar.gz "$INCOMING"/*.tgz "$INCOMING"/*.tar; do
    [ -f "$archive" ] || continue
    case "$archive" in
      *.tar.gz) name="$(basename "$archive" .tar.gz)" ;;
      *.tgz) name="$(basename "$archive" .tgz)" ;;
      *) name="$(basename "$archive" .tar)" ;;
    esac
    dest="$INCOMING/extracted/$name"
    mkdir -p "$dest"
    tar -xf "$archive" -C "$dest"
  done

  shopt -u nullglob
fi

# --- 3. metadata summary -----------------------------------------------------
META="$INCOMING/.wargame.json"
README="$NOTES/README.md"

if [ -f "$META" ]; then
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$META" "$README" <<'PYEOF'
import json
import sys

meta_path, out_path = sys.argv[1], sys.argv[2]
with open(meta_path, encoding="utf-8") as f:
    meta = json.load(f)

lines = ["# Challenge notes", ""]
lines.append(f"- provider: {meta.get('provider', 'unknown')}")
lines.append(f"- challenge id: {meta.get('challenge_id', 'unknown')}")
lines.append(f"- title: {meta.get('title', 'unknown')}")
sha = meta.get("download_sha256") or {}
if sha:
    lines.append("- files:")
    for name, digest in sha.items():
        lines.append(f"  - {name}: {digest}")
lines.append("")
lines.append("Downloaded files were NOT executed automatically. Review "
             "`incoming/` before running anything.")
lines.append("")

with open(out_path, "w", encoding="utf-8") as f:
    f.write("\n".join(lines))
PYEOF
  else
    {
      echo "# Challenge notes"
      echo
      echo "(python3 unavailable in this guest; showing the raw metadata)"
      echo
      echo '```json'
      cat "$META"
      echo
      echo '```'
    } >"$README"
  fi
else
  {
    echo "# Challenge notes"
    echo
    echo "No metadata file was found at incoming/.wargame.json."
  } >"$README"
fi

# --- 4. welcome banner --------------------------------------------------------
cat <<BANNER

==============================================================
 wargame-vm: workspace ready
==============================================================
 Workspace : $WORKSPACE
 Incoming  : $INCOMING
 Work      : $WORK
 Notes     : $README
 Output    : $OUTPUT

 Downloaded challenge files were NOT executed automatically.
 Review incoming/ before running anything.
==============================================================

BANNER
