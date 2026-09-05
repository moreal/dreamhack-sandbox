#!/usr/bin/env bash
# wargame-vm profile provisioning: common baseline (DESIGN.md §6, MVP.md §13).
#
# Runs once, inside the profile image being built (never inside a live
# challenge session). Idempotent: safe to re-run against an already
# provisioned image.

set -euo pipefail

echo "==> common.sh: baseline provisioning"

# --- Homebrew ----------------------------------------------------------------
if ! command -v brew >/dev/null 2>&1; then
  echo "==> installing Homebrew"
  NONINTERACTIVE=1 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
fi

# shellcheck disable=SC2016 # intentionally literal: expanded when sourced by the shell, not here.
BREW_SHELLENV='eval "$(/opt/homebrew/bin/brew shellenv)"'
touch "$HOME/.zprofile"
if ! grep -qF "$BREW_SHELLENV" "$HOME/.zprofile"; then
  echo "$BREW_SHELLENV" >>"$HOME/.zprofile"
fi

# A non-login, non-interactive shell -- which is what sshd runs for
# `ssh host -- cmd` (including every wargame-vm `GuestTransport::exec` call)
# -- only sources ~/.zshenv, never ~/.zprofile. Without brew's shellenv also
# landing there, /opt/homebrew/bin is missing from PATH for every such
# command, so tools installed via `brew install` (e.g. uv) resolve to
# "command not found" (exit 127) even though they're present on disk.
touch "$HOME/.zshenv"
if ! grep -qF "$BREW_SHELLENV" "$HOME/.zshenv"; then
  echo "$BREW_SHELLENV" >>"$HOME/.zshenv"
fi

eval "$(/opt/homebrew/bin/brew shellenv)"

echo "==> brew install: baseline packages"
brew install git python uv curl wget jq vim tree

# --- keep the guest awake while it is unattended-provisioned/used -----------
echo "==> disabling sleep and display sleep"
sudo pmset -a sleep 0 displaysleep 0

# --- workspace root ------------------------------------------------------------
mkdir -p "$HOME/wargame"

echo "==> common.sh: done"
echo "common.sh: PATH=$PATH"
