#!/usr/bin/env bash
# wargame-vm guest bootstrap for POSIX guests -- macOS and Kali/Debian
# (MVP.md §15, DESIGN.md §6/§3.4, spec.md §3 FR-4).
#
# Runs *inside* the disposable guest, never on the host. Responsibilities:
#   1. Create the challenge workspace layout (incoming/work/notes/output).
#   2. Verify every transferred file against the SHA-256 the host recorded
#      while staging it (incoming/.wargame-sha256).
#   3. Optionally unpack known archive types already staged under incoming/
#      -- guest-side only, and only after every entry in the archive has
#      passed the safety rules below.
#   4. Summarize incoming/.wargame.json (if the host wrote one) into a
#      human-readable notes/README.md.
#   5. Print a short welcome banner.
#
# It NEVER executes anything found inside incoming/ -- extraction only.
#
# Portability: written for bash 3.2, which is what macOS ships as
# /bin/bash; Kali/Debian's bash 5 runs the same script. Only tools present
# on both are used (tar, sed, tr, and one of sha256sum/shasum/openssl);
# anything optional (unzip, python3) is probed before use and its absence is
# reported rather than assumed.
#
# Usage: bootstrap.sh <workspace-dir> --extract=<true|false>
#        bootstrap.sh --check-entry <archive-entry-name>

set -euo pipefail

usage() {
  echo "usage: $(basename "$0") <workspace-dir> --extract=<true|false>" >&2
  echo "       $(basename "$0") --check-entry <archive-entry-name>" >&2
  exit 2
}

# --- archive entry safety (spec.md §3 FR-4, §7 AC-11) -----------------------
#
# One rule set, applied to every entry name in an archive *before* anything
# is unpacked. An archive with a single failing entry is not extracted at
# all: the original stays in incoming/ and the reason is reported. The rules
# are deliberately identical to guest/bootstrap.ps1's, so a challenge behaves
# the same on every guest OS.
#
# Rejected: absolute paths, `..` segments, Windows drive letters and UNC
# prefixes, backslash-escaped traversal, `:` (drive/alternate data stream),
# reserved Windows device names, components ending in a dot or space, and
# control characters (which also mean an archive listing cannot be trusted).

RESERVED_NAMES="CON PRN AUX NUL COM1 COM2 COM3 COM4 COM5 COM6 COM7 COM8 COM9 LPT1 LPT2 LPT3 LPT4 LPT5 LPT6 LPT7 LPT8 LPT9"

entry_reason=""

check_entry() {
  local name norm stripped rest component base upper
  entry_reason=""
  name="$1"

  if [ -z "$name" ]; then
    entry_reason="empty entry name"
    return 1
  fi
  case "$name" in
    *[[:cntrl:]]*)
      entry_reason="control character in the name"
      return 1
      ;;
  esac

  # A backslash is a path separator on Windows, so `..\evil` and
  # `\\server\share\evil` are traversal too: normalise before splitting.
  norm="${name//\\//}"
  case "$norm" in
    /*)
      entry_reason="absolute path"
      return 1
      ;;
  esac

  # Directory entries end with a separator; drop exactly one.
  stripped="$norm"
  case "$stripped" in
    */) stripped="${stripped%/}" ;;
  esac
  if [ -z "$stripped" ]; then
    entry_reason="empty entry name"
    return 1
  fi

  rest="$stripped"
  while [ -n "$rest" ]; do
    component="${rest%%/*}"
    if [ "$rest" = "$component" ]; then
      rest=""
    else
      rest="${rest#*/}"
    fi

    if [ -z "$component" ]; then
      entry_reason="empty path component"
      return 1
    fi
    if [ "$component" = ".." ]; then
      entry_reason="parent-directory traversal (..)"
      return 1
    fi
    case "$component" in
      *:*)
        entry_reason="\"$component\" carries a drive letter or alternate data stream"
        return 1
        ;;
    esac
    if [ "$component" != "." ]; then
      case "$component" in
        *. | *' ')
          entry_reason="\"$component\" ends with a dot or space"
          return 1
          ;;
      esac
    fi
    base="${component%%.*}"
    upper=$(printf '%s' "$base" | tr 'abcdefghijklmnopqrstuvwxyz' 'ABCDEFGHIJKLMNOPQRSTUVWXYZ')
    case " $RESERVED_NAMES " in
      *" $upper "*)
        entry_reason="\"$component\" is a reserved Windows device name"
        return 1
        ;;
    esac
  done
  return 0
}

# `--check-entry <name>`: apply the rule above and say nothing else. This is
# the seam the host-side tests use to assert both bootstrap scripts agree,
# and it creates nothing.
if [ "${1:-}" = "--check-entry" ]; then
  shift
  if [ "$#" -ne 1 ]; then
    usage
  fi
  if check_entry "$1"; then
    exit 0
  fi
  echo "unsafe archive entry \"$1\": $entry_reason" >&2
  exit 1
fi

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

# --- 2. host/guest SHA-256 agreement ----------------------------------------
#
# spec.md §3 FR-4: "게스트 파일 해시는 호스트 staging 해시와 같아야 한다".
# The host writes incoming/.wargame-sha256 in `sha256sum` layout
# ("<64 hex>  <filename>") while staging, so nothing here has to parse JSON.

DIGESTS="$INCOMING/.wargame-sha256"

sha256_of() {
  case "$SHA256_TOOL" in
    sha256sum) sha256sum "$1" | cut -d' ' -f1 ;;
    shasum) shasum -a 256 "$1" | cut -d' ' -f1 ;;
    openssl) openssl dgst -sha256 "$1" | sed 's/.*= *//' ;;
  esac
}

if [ -f "$DIGESTS" ] && [ -s "$DIGESTS" ]; then
  SHA256_TOOL=""
  if command -v sha256sum >/dev/null 2>&1; then
    SHA256_TOOL="sha256sum"
  elif command -v shasum >/dev/null 2>&1; then
    SHA256_TOOL="shasum"
  elif command -v openssl >/dev/null 2>&1; then
    SHA256_TOOL="openssl"
  fi
  if [ -z "$SHA256_TOOL" ]; then
    echo "no SHA-256 tool (sha256sum, shasum or openssl) in this guest; cannot confirm the transferred files" >&2
    exit 1
  fi

  hash_failures=0
  while IFS= read -r line || [ -n "$line" ]; do
    [ -n "$line" ] || continue
    expected=$(printf '%s' "$line" | cut -c1-64)
    file=$(printf '%s' "$line" | cut -c67-)
    if [ -z "$file" ]; then
      continue
    fi
    if [ ! -f "$INCOMING/$file" ]; then
      echo "missing transferred file: $file" >&2
      hash_failures=$((hash_failures + 1))
      continue
    fi
    actual=$(sha256_of "$INCOMING/$file")
    if [ "$actual" != "$expected" ]; then
      echo "SHA-256 mismatch for $file: host recorded $expected, guest computed $actual" >&2
      hash_failures=$((hash_failures + 1))
    fi
  done <"$DIGESTS"

  if [ "$hash_failures" -ne 0 ]; then
    echo "$hash_failures transferred file(s) do not match the host's SHA-256 digests; nothing was extracted" >&2
    exit 1
  fi
fi

# --- 3. optional guest-side extraction --------------------------------------
#
# Extraction happens only in the guest, and only after every entry name has
# passed check_entry and the archive has been confirmed link-free. A refused
# archive keeps its original file and creates no destination directory at
# all, so a rejection can never leave a half-unpacked tree behind.

REFUSALS="$NOTES/archive-refusals.txt"
# A re-run reports what *this* run refused, not a union with the last one.
rm -f "$REFUSALS"
archive_reason=""

archive_is_safe() {
  local archive kind listing entry
  archive="$1"
  kind="$2"
  archive_reason=""

  if [ "$kind" = "zip" ]; then
    if ! command -v unzip >/dev/null 2>&1; then
      archive_reason="no unzip in this guest, so its entries cannot be inspected"
      return 1
    fi
    if ! listing=$(unzip -Z1 "$archive" 2>/dev/null); then
      archive_reason="the archive could not be listed (is it a valid zip?)"
      return 1
    fi
    # `unzip -Z`'s long listing starts every entry line with its mode
    # string; a symbolic link entry starts with "l". Zip has no hard links.
    if unzip -Z "$archive" 2>/dev/null | grep -q '^l'; then
      archive_reason="it contains a symbolic link entry, which is never extracted"
      return 1
    fi
  else
    if ! listing=$(tar -tf "$archive" 2>/dev/null); then
      archive_reason="the archive could not be listed (is it a valid tar?)"
      return 1
    fi
    # tar's verbose listing marks symbolic links "l" and hard links "h".
    if tar -tvf "$archive" 2>/dev/null | cut -c1 | grep -q '^[lh]$'; then
      archive_reason="it contains a symbolic or hard link entry, which is never extracted"
      return 1
    fi
  fi

  if [ -n "$listing" ]; then
    while IFS= read -r entry || [ -n "$entry" ]; do
      [ -n "$entry" ] || continue
      if ! check_entry "$entry"; then
        archive_reason="entry \"$entry\": $entry_reason"
        return 1
      fi
    done <<EOF
$listing
EOF
  fi
  return 0
}

refuse_archive() {
  echo "refusing to extract $(basename "$1"): $2" >&2
  echo "$(basename "$1"): $2" >>"$REFUSALS"
}

if [ "$EXTRACT" = "true" ]; then
  shopt -s nullglob

  for archive in "$INCOMING"/*.zip "$INCOMING"/*.tar.gz "$INCOMING"/*.tgz "$INCOMING"/*.tar; do
    [ -f "$archive" ] || continue
    case "$archive" in
      *.zip)
        kind="zip"
        name="$(basename "$archive" .zip)"
        ;;
      *.tar.gz)
        kind="tar"
        name="$(basename "$archive" .tar.gz)"
        ;;
      *.tgz)
        kind="tar"
        name="$(basename "$archive" .tgz)"
        ;;
      *)
        kind="tar"
        name="$(basename "$archive" .tar)"
        ;;
    esac

    if ! archive_is_safe "$archive" "$kind"; then
      refuse_archive "$archive" "$archive_reason"
      continue
    fi

    dest="$INCOMING/extracted/$name"
    mkdir -p "$dest"
    if [ "$kind" = "zip" ]; then
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
    else
      tar -xf "$archive" -C "$dest"
    fi
  done

  shopt -u nullglob
fi

# --- 4. metadata summary -----------------------------------------------------
META="$INCOMING/.wargame.json"
README="$NOTES/README.md"

summarize_raw() {
  {
    echo "# Challenge notes"
    echo
    echo "(no working python3 in this guest; showing the raw metadata)"
    echo
    echo '```json'
    cat "$META"
    echo
    echo '```'
  } >"$README"
}

if [ -f "$META" ]; then
  # A guest without a working python3 (a bare macOS image, say) still gets
  # its notes: the summary is a convenience, never a reason to fail a
  # launch, so the interpreter's own exit status is handled rather than
  # left to `set -e`.
  if command -v python3 >/dev/null 2>&1 &&
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
  then
    :
  else
    summarize_raw
  fi
else
  {
    echo "# Challenge notes"
    echo
    echo "No metadata file was found at incoming/.wargame.json."
  } >"$README"
fi

# --- 5. welcome banner --------------------------------------------------------
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

if [ -f "$REFUSALS" ]; then
  echo " Some archives were left packed; see $REFUSALS"
  echo
fi
