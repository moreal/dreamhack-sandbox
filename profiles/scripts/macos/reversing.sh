#!/usr/bin/env bash
# wargame-vm profile provisioning: reversing tools, on top of common.sh
# (DESIGN.md §6, MVP.md §13). Idempotent.

set -euo pipefail

echo "==> reversing.sh: reverse-engineering tooling"

eval "$(/opt/homebrew/bin/brew shellenv)"

# Ghidra needs a JRE/JDK; Temurin is a well-maintained OpenJDK cask.
echo "==> brew install --cask temurin"
brew install --cask temurin

echo "==> brew install: reversing packages"
brew install ghidra llvm binutils hexyl file

if ! command -v ghidraRun >/dev/null 2>&1 && [ ! -x /opt/homebrew/bin/ghidraRun ]; then
  echo "==> ghidra formula did not provide ghidraRun, falling back to the cask"
  brew install --cask ghidra
fi

echo "==> reversing.sh: done"
