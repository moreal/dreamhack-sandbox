#!/usr/bin/env bash
# wargame-vm profile provisioning: Kali Linux baseline (spec.md §3 FR-3).
#
# Runs once, inside the profile image being built (never inside a live
# challenge session), over SSH as the profile's `guest.user`. Must be
# idempotent: `wargame profile build` re-runs it against an already
# provisioned image, and every step below is a no-op the second time.
#
# Everything is installed from Kali's own apt repositories over the build's
# NAT network. Nothing is downloaded from a third-party URL and nothing is
# piped into a shell.
#
#   Tool             Install source                    Verify command
#   ---------------  -------------------------------   -----------------------
#   Python 3         apt-get install python3           python3 --version
#   pip              apt-get install python3-pip       python3 -m pip --version
#   venv             apt-get install python3-venv      python3 -m venv --help
#   Git              apt-get install git               git --version
#   curl             apt-get install curl              curl --version
#   unzip            apt-get install unzip             unzip -v
#   file             apt-get install file              file --version
#   GDB              apt-get install gdb               gdb --version
#   binutils         apt-get install binutils          objdump --version
#   build-essential  apt-get install build-essential   gcc --version
#                                                      make --version
#
# The verify column is the profile's own `tools.verify` list: `wargame
# profile build` runs those commands again, from the host, after this script
# finishes, and refuses to publish the image if any of them fails. They are
# re-run here too so the provisioning log records each one and its exit code
# next to the install that produced it.

set -euo pipefail

trap 'printf "!!! kali/common.sh failed at line %s (exit %s)\n" "$LINENO" "$?" >&2' ERR

log() { printf '==> %s\n' "$*"; }

# Runs one labelled step, logging its exit code either way. `set -e` makes a
# non-zero return here abort the script, which fails the build.
step() {
  local label="$1"
  shift
  log "$label"
  local status=0
  "$@" || status=$?
  printf '<== %s: exit %s\n' "$label" "$status"
  return "$status"
}

# Runs one of the profile's verify commands, logging the command and its exit
# code. A failure here fails the build at the step that caused it rather than
# at the host's later verification pass.
verify() {
  local status=0
  "$@" || status=$?
  printf '    verify: %s -> exit %s\n' "$*" "$status"
  return "$status"
}

log "kali/common.sh: baseline provisioning for $(id -un)@$(hostname)"

# --- prerequisites ----------------------------------------------------------
# apt needs root. The build connects as the profile's unprivileged guest
# user, so that user has to be able to sudo without a password prompt -- an
# interactive prompt over a non-interactive SSH command would simply hang
# until the build's timeout.
if ! sudo -n true 2>/dev/null; then
  cat >&2 <<EOF
kali/common.sh: this build needs passwordless sudo for $(id -un).
Open the installation VM (wargame profile console <profile>) and run:

  echo '$(id -un) ALL=(ALL) NOPASSWD:ALL' | sudo tee /etc/sudoers.d/wargame
  sudo chmod 0440 /etc/sudoers.d/wargame

then run \`wargame profile build <profile>\` again.
EOF
  exit 1
fi

# `apt-get` under a non-interactive SSH command must never open a dialog or
# stop on a modified config file, so the frontend is fixed and dpkg is told
# to keep the packaged answer.
export DEBIAN_FRONTEND=noninteractive
apt_get=(
  sudo -n env DEBIAN_FRONTEND=noninteractive
  apt-get -y
  -o Dpkg::Options::=--force-confdef
  -o Dpkg::Options::=--force-confold
)

packages=(
  python3
  python3-pip
  python3-venv
  git
  curl
  unzip
  file
  gdb
  binutils
  build-essential
)

# --- install ----------------------------------------------------------------
step "apt-get update" "${apt_get[@]}" update
step "apt-get install: ${packages[*]}" "${apt_get[@]}" install "${packages[@]}"

# --- workspace root ---------------------------------------------------------
# The profile's `workspace.root`; each launched challenge gets its own
# subdirectory underneath it.
step "workspace root" mkdir -p "$HOME/wargame"

# --- verification -----------------------------------------------------------
log "verifying the installed tools"
verify python3 --version
verify python3 -m pip --version
verify python3 -m venv --help
verify git --version
verify curl --version
verify unzip -v
verify file --version
verify gdb --version
verify objdump --version
verify gcc --version
verify make --version

log "kali/common.sh: done"
