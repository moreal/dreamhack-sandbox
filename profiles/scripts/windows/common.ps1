# wargame-vm profile provisioning: Windows baseline.
#
# Runs once, inside the profile image being built (never inside a live
# challenge session), over SSH as the profile's `guest.user`, under
# PowerShell. Must be idempotent: `wargame profile build` re-runs it against
# an already provisioned image.
#
# TODO(Task 5): PowerShell execution over the guest transport.
# TODO(Task 6): the real Windows tool set -- OpenSSH Server, Python 3, Git,
# 7-Zip -- with the install source and a verification command for each
# (spec.md §3 FR-3).

$ErrorActionPreference = 'Stop'

Write-Host '==> windows/common.ps1: placeholder, installs nothing yet'
