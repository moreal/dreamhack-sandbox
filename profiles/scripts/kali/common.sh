#!/usr/bin/env bash
# wargame-vm profile provisioning: Kali Linux baseline.
#
# Runs once, inside the profile image being built (never inside a live
# challenge session), over SSH as the profile's `guest.user`. Must be
# idempotent: `wargame profile build` re-runs it against an already
# provisioned image.
#
# TODO(Task 6): the real Kali tool set -- Python 3 + pip/venv, Git, curl,
# unzip, file, GDB, binutils, build-essential -- with the install source and
# a verification command for each (spec.md §3 FR-3).

set -euo pipefail

echo "==> kali/common.sh: placeholder, installs nothing yet"
