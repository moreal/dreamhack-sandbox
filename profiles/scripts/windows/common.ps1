# wargame-vm profile provisioning: Windows baseline (spec.md §3 FR-3).
#
# Runs once, inside the profile image being built (never inside a live
# challenge session), over SSH as the profile's `guest.user`, under
# PowerShell. Must be idempotent: `wargame profile build` re-runs it against
# an already provisioned image, and every step below checks before it acts.
#
#   Tool            Install source                                Verify command
#   --------------  --------------------------------------------  ---------------------------------------------------------------------
#   PowerShell      ships with Windows                            powershell -NoProfile -Command $PSVersionTable.PSVersion.ToString()
#   OpenSSH Server  Windows capability OpenSSH.Server             sc.exe query sshd
#   Python 3        winget install Python.Python.3.12             python --version
#   Git             winget install Git.Git                        git --version
#   7-Zip           winget install 7zip.7zip                      7z i
#
# The verify column is the profile's own `tools.verify` list: `wargame
# profile build` runs those commands again, from the host, after this script
# finishes, and refuses to publish the image if any of them fails. Each is
# re-run here so the provisioning log records the command and its exit code
# next to the install that produced it.
#
# Those host-side verify commands are argv, not a shell line, and every token
# is passed to the guest as a quoted literal. A PowerShell *cmdlet* binds a
# parameter only from a bare `-Name` token, so a cmdlet there would misbind
# its arguments; every entry above is therefore a native executable
# (`powershell.exe`, `sc.exe`, `python.exe`, `git.exe`, `7z.exe`). Inside
# *this* file there is no such restriction -- it runs as a whole script.
#
# Exit codes: 0 is success, 3010 means "reboot me, then carry on" (the build
# waits for the guest to go down and come back). Any other non-zero code
# fails the build. A code outside 0-255 cannot survive `ssh`'s 8-bit exit
# status, so wargame's wrapper reports it on a `wargame-exit-code:<n>` stderr
# line and exits 255; the host puts the real value back and strips the line.
# A script here therefore just `exit 3010` -- nothing extra to do -- but must
# not print a line starting with `wargame-exit-code:` itself. Because the
# build resumes with the *next* script rather than re-running this one, the
# 3010 is deferred to the very end: everything is installed first.

$ErrorActionPreference = 'Stop'

$script:RebootRequired = $false

function Write-Step {
    param([Parameter(Mandatory)][string] $Message)
    Write-Host "==> $Message"
}

function Write-Result {
    param([Parameter(Mandatory)][string] $Label, [Parameter(Mandatory)][int] $Code)
    Write-Host "<== ${Label}: exit $Code"
}

# Runs a native command and returns nothing but its exit code.
#
# Two things matter here. `| Out-Host` (or `*> $null`) rather than letting
# the command write to the pipeline: this returns the exit code, and anything
# the command printed would otherwise come back alongside it as an array.
# And `$ErrorActionPreference = 'Continue'` for the duration: in Windows
# PowerShell 5.1 a native command that writes to stderr while its streams are
# redirected raises a NativeCommandError, which under 'Stop' would be
# reported as a failure for the very ordinary reason that a tool printed its
# version banner to stderr. The exit code is the answer; the error stream is
# not. (wargame's own command wrapper dodges the same trap the same way.)
function Invoke-NativeExitCode {
    param(
        [Parameter(Mandatory)][string] $Command,
        [string[]]                     $Arguments = @(),
        [switch]                       $Quiet
    )
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $global:LASTEXITCODE = 0
    try {
        if ($Quiet) {
            & $Command @Arguments *> $null
        } else {
            & $Command @Arguments | Out-Host
        }
        return $LASTEXITCODE
    } catch {
        # The command could not be started at all (not on PATH, say).
        Write-Host "    ${Command}: $($_.Exception.Message)"
        return 1
    } finally {
        $ErrorActionPreference = $previous
    }
}

# Runs a native command, logs its exit code, and fails the build unless the
# code is one this step accepts.
function Invoke-Native {
    param(
        [Parameter(Mandatory)][string]   $Label,
        [Parameter(Mandatory)][string]   $Command,
        [string[]]                       $Arguments = @(),
        [int[]]                          $AllowedExitCodes = @(0),
        [switch]                         $AllowAnyExitCode
    )
    Write-Step $Label
    $code = Invoke-NativeExitCode -Command $Command -Arguments $Arguments
    Write-Result $Label $code
    if (-not $AllowAnyExitCode -and $AllowedExitCodes -notcontains $code) {
        throw "$Label failed (exit $code)"
    }
    return $code
}

# True when a verify command succeeds, i.e. the tool is already usable. This
# is deliberately the same command the profile verifies with, so "already
# installed" and "verified" can never disagree.
function Test-Tool {
    param([Parameter(Mandatory)][string] $Command, [string[]] $Arguments = @())
    return (Invoke-NativeExitCode -Command $Command -Arguments $Arguments -Quiet) -eq 0
}

# Re-reads PATH from the registry so a tool installed a moment ago is
# reachable in this same session.
function Update-SessionPath {
    $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $user    = [Environment]::GetEnvironmentVariable('Path', 'User')
    $env:Path = (@($machine, $user) | Where-Object { $_ }) -join ';'
}

# Adds a directory to the *machine* PATH if it is not already there, so the
# next SSH session -- which is what runs `tools.verify` -- can see it.
function Add-MachinePath {
    param([Parameter(Mandatory)][string] $Directory)
    if (-not (Test-Path -LiteralPath $Directory)) { return }
    $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $entries = @($machine -split ';' | Where-Object { $_ })
    if ($entries -contains $Directory) { return }
    Write-Step "adding $Directory to the machine PATH"
    [Environment]::SetEnvironmentVariable('Path', (($entries + $Directory) -join ';'), 'Machine')
    Update-SessionPath
}

Write-Host "==> windows/common.ps1: baseline provisioning for $env:USERNAME on $env:COMPUTERNAME"

# --- prerequisites ----------------------------------------------------------
# Installing a Windows capability and writing the machine PATH both need an
# elevated token. OpenSSH gives a member of the local Administrators group
# one automatically; a standard user cannot be provisioned, and finding that
# out here is far cheaper than failing halfway through.
$identity  = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw @"
windows/common.ps1: $env:USERNAME is not an administrator in this session.
Open the installation VM (wargame profile console <profile>), add the account
to the local Administrators group, then run ``wargame profile build`` again.
"@
}

# --- PowerShell -------------------------------------------------------------
# Ships with Windows; nothing to install. Recorded so the log names the
# version the image was built with.
Invoke-Native 'PowerShell (ships with Windows)' 'powershell' `
    @('-NoProfile', '-Command', '$PSVersionTable.PSVersion.ToString()') | Out-Null

# --- OpenSSH Server ---------------------------------------------------------
# This script is itself running over SSH, so the server is already there --
# but the *image* has to come back with it enabled after a reboot, and with
# PowerShell as the shell every wargame command is phrased for.
Write-Step 'OpenSSH Server (Windows capability OpenSSH.Server)'
$capability = Get-WindowsCapability -Online -Name 'OpenSSH.Server*' |
    Select-Object -First 1
if ($capability -and $capability.State -ne 'Installed') {
    $result = Add-WindowsCapability -Online -Name $capability.Name
    if ($result.RestartNeeded) {
        Write-Host '    OpenSSH Server asked for a restart'
        $script:RebootRequired = $true
    }
}
Set-Service -Name sshd -StartupType Automatic
if ((Get-Service -Name sshd).Status -ne 'Running') {
    Start-Service -Name sshd
}
$shellKey = 'HKLM:\SOFTWARE\OpenSSH'
if (-not (Test-Path -LiteralPath $shellKey)) {
    New-Item -Path $shellKey -Force | Out-Null
}
New-ItemProperty -Path $shellKey -Name 'DefaultShell' `
    -Value "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" `
    -PropertyType String -Force | Out-Null
Write-Result 'OpenSSH Server' 0

# --- winget -----------------------------------------------------------------
# The three remaining tools come from winget, the Windows package manager
# that ships with the App Installer. Windows Server images and some stripped
# installations do not have it; there is no safe silent fallback (downloading
# installers over plain HTTP and running them unattended is exactly what this
# project refuses to do), so say what to install and stop.
if (-not (Get-Command 'winget' -ErrorAction SilentlyContinue)) {
    throw @"
windows/common.ps1: winget was not found, so Python, Git and 7-Zip cannot be
installed. Open the installation VM (wargame profile console <profile>),
install "App Installer" from the Microsoft Store (or the package from
https://aka.ms/getwinget), confirm ``winget --version`` works in the guest,
then run ``wargame profile build`` again.
"@
}

# `--disable-interactivity` and the two agreement flags matter: this runs as
# a non-interactive SSH command, and a prompt would hang until the build
# times out. 1641 and 3010 are the installer conventions for "done, but the
# machine has to restart".
$wingetCommon = @(
    '--exact',
    '--silent',
    '--disable-interactivity',
    '--accept-source-agreements',
    '--accept-package-agreements'
)

function Install-WingetPackage {
    param(
        [Parameter(Mandatory)][string] $Label,
        [Parameter(Mandatory)][string] $Id,
        [Parameter(Mandatory)][string] $ProbeCommand,
        [string[]]                     $ProbeArguments = @()
    )
    Update-SessionPath
    if (Test-Tool -Command $ProbeCommand -Arguments $ProbeArguments) {
        Write-Step "$Label: already installed"
        Write-Result $Label 0
        return
    }
    # Any exit code is accepted here, and the probe below decides. winget has
    # a whole family of "nothing to do" codes -- already installed, no
    # applicable update, a newer version present -- and hard-coding them
    # would turn a re-run into a failed build the day one of them changes.
    # What the build actually needs to know is whether the tool runs now.
    $code = Invoke-Native "$Label (winget install $Id)" 'winget' `
        (@('install', '--id', $Id) + $wingetCommon) -AllowAnyExitCode
    if ($code -eq 1641 -or $code -eq 3010) {
        Write-Host "    $Label asked for a restart"
        $script:RebootRequired = $true
        return
    }
    Update-SessionPath
    if (-not (Test-Tool -Command $ProbeCommand -Arguments $ProbeArguments)) {
        throw "${Label}: winget exited $code and ``$ProbeCommand $($ProbeArguments -join ' ')`` still does not run"
    }
}

Install-WingetPackage -Label 'Python 3' -Id 'Python.Python.3.12' `
    -ProbeCommand 'python' -ProbeArguments @('--version')
Install-WingetPackage -Label 'Git' -Id 'Git.Git' `
    -ProbeCommand 'git' -ProbeArguments @('--version')
Install-WingetPackage -Label '7-Zip' -Id '7zip.7zip' `
    -ProbeCommand '7z' -ProbeArguments @('i')

# winget's 7-Zip package does not put 7z.exe on PATH, and `tools.verify` runs
# it by name in a fresh session.
Add-MachinePath -Directory "$env:ProgramFiles\7-Zip"

# The workspace root is deliberately not created here: it comes from the
# profile's `workspace.root`, which this script cannot see, and every launch
# creates it (and the challenge's subdirectory under it) in the guest anyway.
# Hard-coding `C:\wargame` would silently diverge the moment someone edits
# the profile.

# --- verification -----------------------------------------------------------
# This runs *before* the `exit 3010` below, so a tool that only becomes
# verifiable after the restart would fail here rather than taking the reboot
# path. That is fine for the five checks this script ships -- PowerShell,
# sshd, Python, Git and 7-Zip are all usable the moment their installer
# finishes -- but a tool you add that genuinely needs the reboot belongs in a
# script of its own, listed after this one, so the build reboots in between.
Write-Step 'verifying the installed tools'
Update-SessionPath
$checks = @(
    @{ Label = 'powershell -NoProfile -Command $PSVersionTable.PSVersion.ToString()'
       Command = 'powershell'
       Arguments = @('-NoProfile', '-Command', '$PSVersionTable.PSVersion.ToString()') },
    # `sc.exe query` exits 0 for a service that exists but is stopped, and
    # prints its STATE either way. That it is *running* needs no separate
    # check: this script is talking to the guest over that very service.
    @{ Label = 'sc.exe query sshd'; Command = 'sc.exe'; Arguments = @('query', 'sshd') },
    @{ Label = 'python --version';  Command = 'python'; Arguments = @('--version') },
    @{ Label = 'git --version';     Command = 'git';    Arguments = @('--version') },
    @{ Label = '7z i';              Command = '7z';     Arguments = @('i') }
)
$failed = @()
foreach ($check in $checks) {
    $code = Invoke-NativeExitCode -Command $check.Command -Arguments $check.Arguments
    Write-Host "    verify: $($check.Label) -> exit $code"
    if ($code -ne 0) { $failed += "$($check.Label) (exit $code)" }
}
if ($failed.Count -gt 0) {
    throw "windows/common.ps1: verification failed: $($failed -join '; ')"
}

# --- done -------------------------------------------------------------------
if ($script:RebootRequired) {
    Write-Host '==> windows/common.ps1: done, but the guest needs a restart (exit 3010)'
    exit 3010
}

Write-Host '==> windows/common.ps1: done'
exit 0
