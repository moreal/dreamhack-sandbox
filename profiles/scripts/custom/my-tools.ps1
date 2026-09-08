# wargame-vm: example of your own provisioning script (Windows guests).
#
# Nothing runs this file as it stands -- it is a starting point to copy. Put
# your own scripts under $WARGAME_HOME/profiles/scripts/ (this file lives at
# scripts/custom/my-tools.ps1) and list them in a profile's
# `provision.scripts` after the baseline one:
#
#   provision:
#     scripts:
#       - scripts/windows/common.ps1
#       - scripts/custom/my-tools.ps1
#   tools:
#     verify:
#       - ...
#       - x64dbg.exe --version
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
# Four rules this script has to keep, because `wargame profile build` runs it
# in the guest with `powershell -File` over a non-interactive SSH command:
#
#   1. Idempotent -- every build re-runs it against the previous image.
#   2. Never interactive -- winget needs --silent --disable-interactivity and
#      the agreement flags, or it hangs until the build's timeout.
#   3. Non-zero exit fails the build, and no image is published. Log each
#      step and its exit code so the provisioning log
#      ($WARGAME_HOME/logs/provision-<profile>.log) says what happened.
#   4. `exit 3010` means "installed, but restart me first": the build waits
#      for the guest to come back and moves on to the *next* script, so do
#      all of this script's work before exiting 3010.
#
# Add a verify command per tool to the profile's `tools.verify`. Those are
# argv, not a shell line, and every token reaches the guest as a quoted
# literal -- so a verify entry has to be a **native executable**
# (`something.exe --version`), never a PowerShell cmdlet, which would bind
# its quoted parameter names as positional values. Inside this file cmdlets
# are fine: it runs as a whole script.

$ErrorActionPreference = 'Stop'

function Write-Step {
    param([Parameter(Mandatory)][string] $Message)
    Write-Host "==> $Message"
}

function Invoke-Native {
    param(
        [Parameter(Mandatory)][string] $Label,
        [Parameter(Mandatory)][string] $Command,
        [string[]]                     $Arguments = @(),
        [int[]]                        $AllowedExitCodes = @(0)
    )
    Write-Step $Label
    & $Command @Arguments
    $code = $LASTEXITCODE
    Write-Host "<== ${Label}: exit $code"
    if ($AllowedExitCodes -notcontains $code) {
        throw "$Label failed (exit $code)"
    }
    return $code
}

Write-Host '==> my-tools.ps1: custom tools'

# --- example: another winget package -----------------------------------------
#   verify command:  x64dbg.exe --version
#
# $code = Invoke-Native 'x64dbg (winget install x64dbg.x64dbg)' 'winget' @(
#     'install', '--id', 'x64dbg.x64dbg',
#     '--exact', '--silent', '--disable-interactivity',
#     '--accept-source-agreements', '--accept-package-agreements'
# ) @(0, 1641, 3010)

# --- example: a Python tool ---------------------------------------------------
#   verify command:  pwn --version
#
# Invoke-Native 'pip install: pwntools' 'python' @('-m', 'pip', 'install', '--upgrade', 'pwntools')

# --- putting a tool on PATH ---------------------------------------------------
# `tools.verify` runs in a *fresh* SSH session, so a directory added to the
# machine PATH here is visible to it -- but $env:Path in this session is not
# updated for you.
#
# $dir = "$env:ProgramFiles\x64dbg\release\x64"
# $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
# if (($machine -split ';') -notcontains $dir) {
#     [Environment]::SetEnvironmentVariable('Path', "$machine;$dir", 'Machine')
# }

Write-Host '==> my-tools.ps1: done'
exit 0
