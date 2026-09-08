# wargame-vm profile provisioning: Windows baseline.
#
# Runs once, inside the profile image being built (never inside a live
# challenge session), over SSH as the profile's `guest.user`, under
# PowerShell. Must be idempotent: `wargame profile build` re-runs it against
# an already provisioned image.
#
# TODO(Task 6): the real Windows tool set -- OpenSSH Server, Python 3, Git,
# 7-Zip -- with the install source and a verification command for each
# (spec.md §3 FR-3). A script that needs a reboot before the next one runs
# exits 3010; the build waits for the guest to come back and continues.

$ErrorActionPreference = 'Stop'

Write-Host '==> windows/common.ps1: placeholder, installs nothing yet'
