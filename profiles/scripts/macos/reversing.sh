#!/usr/bin/env bash
# wargame-vm profile provisioning: reversing tools, on top of common.sh
# (DESIGN.md §6, MVP.md §13). Idempotent.

set -euo pipefail

echo "==> reversing.sh: reverse-engineering tooling"

eval "$(/opt/homebrew/bin/brew shellenv)"

# The `ghidra` formula brings its own JDK (openjdk@21) and links `ghidraRun`
# and `pyghidraRun` into /opt/homebrew/bin; no separate JDK cask is needed.
# `llvm`, `binutils` and `file` are keg-only, so they are put on PATH below;
# `lld` is a separate formula (not part of `llvm`) and links normally.
echo "==> brew install: reversing packages"
brew install ghidra llvm lld binutils hexyl file

if [ ! -x /opt/homebrew/bin/ghidraRun ]; then
  echo "==> ghidra formula did not provide ghidraRun, falling back to the cask"
  brew install --cask ghidra
fi

# --- PATH for keg-only tools ---------------------------------------------------
# Same two files as common.sh: ~/.zshenv is what a non-login `ssh host cmd`
# shell reads (every wargame-vm `GuestTransport::exec`), ~/.zprofile what an
# interactive `wargame ssh` login reads. Homebrew's LLVM goes first so
# `clang`/`lld`/`llvm-objdump` are the LLVM 20+ ones; GNU binutils goes last
# so Apple's `objdump`/`nm`/`strings` stay the defaults and the GNU ones are
# still reachable by name. Ghidra's headless tools (`analyzeHeadless`) live
# under the keg's `support/` directory.
# shellcheck disable=SC2016 # intentionally literal: expanded when sourced.
# `ghidraRun` is a Homebrew wrapper that points at openjdk@21 itself, but
# the raw `analyzeHeadless` script (and anyone wanting `java`) needs
# JAVA_HOME; openjdk@21 is keg-only too.
# shellcheck disable=SC2016 # intentionally literal: expanded when sourced.
RE_ENV=(
  'export JAVA_HOME="/opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home"'
  'export PATH="/opt/homebrew/opt/llvm/bin:$JAVA_HOME/bin:$PATH:/opt/homebrew/opt/binutils/bin:/opt/homebrew/opt/file-formula/bin:/opt/homebrew/opt/ghidra/libexec/support"'
)
for rc in "$HOME/.zshenv" "$HOME/.zprofile"; do
  touch "$rc"
  for line in "${RE_ENV[@]}"; do
    if ! grep -qF "$line" "$rc"; then
      echo "$line" >>"$rc"
    fi
  done
done

echo "==> reversing.sh: done"
