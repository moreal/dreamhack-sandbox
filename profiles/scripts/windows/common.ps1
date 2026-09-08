# wargame-vm profile provisioning: Windows baseline.
#
# Runs once, inside the profile image being built (never inside a live
# challenge session), over SSH as the profile's `guest.user`, under
# PowerShell. Must be idempotent: `wargame profile build` re-runs it against
# an already provisioned image.
#
# TODO(Task 6): the real Windows tool set -- OpenSSH Server, Python 3, Git,
# 7-Zip -- with the install source and a verification command for each
# (spec.md §3 FR-3).
#
# Exit codes: 0 is success, 3010 means "reboot me, then carry on" (the build
# waits for the guest to go down and come back). Any other non-zero code
# fails the build. A code outside 0-255 cannot survive `ssh`'s 8-bit exit
# status, so wargame's wrapper reports it on a `wargame-exit-code:<n>` stderr
# line and exits 255; the host puts the real value back and strips the line.
# A script here therefore just `exit 3010` -- nothing extra to do -- but must
# not print a line starting with `wargame-exit-code:` itself.

$ErrorActionPreference = 'Stop'

Write-Host '==> windows/common.ps1: placeholder, installs nothing yet'
