#!/usr/bin/env bash
# wargame-vm: example of your own provisioning script (macOS or Kali guests).
#
# Nothing runs this file as it stands -- it is a starting point to copy. Put
# your own scripts under $WARGAME_HOME/profiles/scripts/ (this file lives at
# scripts/custom/my-tools.sh) and list them in a profile's `provision.scripts`
# after the baseline one:
#
#   provision:
#     scripts:
#       - scripts/kali/common.sh
#       - scripts/custom/my-tools.sh
#   tools:
#     verify:
#       - ...
#       - ropper --version
#
# Then rebuild the image:
#
#   wargame profile build <profile>
#
# Anything you add or edit under profiles/ is yours: upgrading wargame
# re-seeds the scripts it ships with, but never overwrites a file you have
# changed. `wargame profile init <template> --name <yours>` additionally
# gives a copied profile its own scripts/<yours>/ directory.
#
# Three rules this script has to keep, because `wargame profile build` runs
# it in the guest over a non-interactive SSH command:
#
#   1. Idempotent -- every build re-runs it against the previous image.
#   2. Never interactive -- a prompt hangs until the build's timeout.
#   3. Non-zero exit fails the build, and no image is published. Log each
#      step and its exit code so the provisioning log
#      ($WARGAME_HOME/logs/provision-<profile>.log) says what happened.
#
# Add a verify command per tool to the profile's `tools.verify`: the build
# re-runs those from the host afterwards and only publishes the image when
# they all pass.

set -euo pipefail

log() { printf '==> %s\n' "$*"; }

step() {
  local label="$1"
  shift
  log "$label"
  local status=0
  "$@" || status=$?
  printf '<== %s: exit %s\n' "$label" "$status"
  return "$status"
}

log "my-tools.sh: custom tools"

# --- example: a Python tool, in its own virtualenv ---------------------------
# Installed with pip into ~/.local/venv/tools, whose bin/ is put on PATH for
# every later SSH session. `pip install` is already idempotent.
#
#   verify command:  ropper --version
#
# VENV="$HOME/.local/venv/tools"
# step "virtualenv $VENV" python3 -m venv "$VENV"
# step "pip install: ropper" "$VENV/bin/pip" install --upgrade ropper
#
# A non-login, non-interactive shell -- which is what `ssh host -- cmd` runs
# -- sources ~/.bashrc on Debian/Kali only through the guard at its top, and
# ~/.profile not at all. Symlinking into ~/.local/bin, which is on the
# default PATH, is the reliable way to expose a tool.
# step "link ropper onto PATH" ln -sf "$VENV/bin/ropper" "$HOME/.local/bin/ropper"

# --- example: a distribution package -----------------------------------------
#   verify command:  radare2 -version
#
# step "apt-get install: radare2" \
#   sudo -n env DEBIAN_FRONTEND=noninteractive apt-get -y install radare2

log "my-tools.sh: done"
