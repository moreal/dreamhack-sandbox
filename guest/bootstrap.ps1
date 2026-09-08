# wargame-vm guest bootstrap for Windows guests (spec.md §3 FR-4, §7 AC-11).
#
# The PowerShell counterpart of guest/bootstrap.sh, with the same contract:
#   1. Create the challenge workspace layout (incoming/work/notes/output).
#   2. Verify every transferred file against the SHA-256 the host recorded
#      while staging it (incoming\.wargame-sha256).
#   3. Optionally unpack known archive types already staged under incoming\
#      -- guest-side only, and only after every entry in the archive has
#      passed the same safety rules bootstrap.sh applies.
#   4. Summarize incoming\.wargame.json into notes\README.md.
#   5. Print a short welcome banner.
#
# It NEVER executes anything found inside incoming\ -- extraction only.
# Nothing that came from a challenge is ever evaluated as code: no
# expression evaluation of guest data, no dot-sourcing.
#
# Runs *inside* the disposable guest, never on the host. Uses only what a
# stock Windows 10/11 install has: PowerShell 5.1, Get-FileHash, .NET's
# System.IO.Compression, and tar.exe (bsdtar, shipped since Windows 10
# 1803). A missing tool is reported, never assumed.
#
# Usage: bootstrap.ps1 -Workspace <dir> -Extract <true|false>
#        bootstrap.ps1 -CheckEntry <archive-entry-name>

[CmdletBinding()]
param(
    [string] $Workspace,
    [string] $Extract = 'false',
    [string] $CheckEntry
)

$ErrorActionPreference = 'Stop'

# --- archive entry safety (spec.md §3 FR-4, §7 AC-11) -----------------------
#
# The same rule set as guest/bootstrap.sh's check_entry, so a challenge
# behaves identically on every guest OS. Returns $null when the entry is safe
# to extract, or the reason it is not.
#
# Rejected: absolute paths, `..` segments, Windows drive letters and UNC
# prefixes, backslash-escaped traversal, ':' (drive letter / alternate data
# stream), reserved Windows device names, components ending in a dot or
# space, and control characters (which also mean an archive listing cannot be
# trusted).

$script:ReservedNames = @('CON', 'PRN', 'AUX', 'NUL') +
    (1..9 | ForEach-Object { "COM$_" }) +
    (1..9 | ForEach-Object { "LPT$_" })

function Test-ArchiveEntry {
    param([string] $Name)

    if ([string]::IsNullOrEmpty($Name)) {
        return 'empty entry name'
    }
    foreach ($char in $Name.ToCharArray()) {
        if ([char]::IsControl($char)) {
            return 'control character in the name'
        }
    }

    # A backslash is a path separator here, so `..\evil` and
    # `\\server\share\evil` are traversal too: normalise before splitting.
    $normalised = $Name.Replace('\', '/')
    if ($normalised.StartsWith('/')) {
        return 'absolute path'
    }

    # Directory entries end with a separator; drop exactly one.
    $stripped = $normalised
    if ($stripped.EndsWith('/')) {
        $stripped = $stripped.Substring(0, $stripped.Length - 1)
    }
    if ([string]::IsNullOrEmpty($stripped)) {
        return 'empty entry name'
    }

    foreach ($component in $stripped.Split('/')) {
        if ([string]::IsNullOrEmpty($component)) {
            return 'empty path component'
        }
        if ($component -eq '..') {
            return 'parent-directory traversal (..)'
        }
        if ($component.Contains(':')) {
            return "`"$component`" carries a drive letter or alternate data stream"
        }
        if ($component -ne '.') {
            if ($component.EndsWith('.') -or $component.EndsWith(' ')) {
                return "`"$component`" ends with a dot or space"
            }
        }
        $base = $component.Split('.')[0]
        if ($script:ReservedNames -contains $base.ToUpperInvariant()) {
            return "`"$component`" is a reserved Windows device name"
        }
    }
    return $null
}

# `-CheckEntry <name>`: apply the rule above and say nothing else. This is
# the seam the host-side tests use to assert both bootstrap scripts agree,
# and it creates nothing.
if ($PSBoundParameters.ContainsKey('CheckEntry')) {
    $reason = Test-ArchiveEntry -Name $CheckEntry
    if ($null -eq $reason) {
        exit 0
    }
    [Console]::Error.WriteLine("unsafe archive entry `"$CheckEntry`": $reason")
    exit 1
}

if ([string]::IsNullOrEmpty($Workspace)) {
    [Console]::Error.WriteLine('usage: bootstrap.ps1 -Workspace <dir> -Extract <true|false>')
    [Console]::Error.WriteLine('       bootstrap.ps1 -CheckEntry <archive-entry-name>')
    exit 2
}
if ($Extract -ne 'true' -and $Extract -ne 'false') {
    [Console]::Error.WriteLine("unrecognized -Extract value: $Extract (expected true or false)")
    exit 2
}

$incoming = Join-Path $Workspace 'incoming'
$work = Join-Path $Workspace 'work'
$notes = Join-Path $Workspace 'notes'
$output = Join-Path $Workspace 'output'

foreach ($dir in @($incoming, $work, $notes, $output)) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}

# --- 2. host/guest SHA-256 agreement ----------------------------------------
#
# spec.md §3 FR-4: "게스트 파일 해시는 호스트 staging 해시와 같아야 한다".
# The host writes incoming\.wargame-sha256 in `sha256sum` layout
# ("<64 hex>  <filename>") while staging, so nothing here has to parse JSON.

$digestFile = Join-Path $incoming '.wargame-sha256'
if (Test-Path -LiteralPath $digestFile) {
    $failures = 0
    foreach ($line in [System.IO.File]::ReadAllLines($digestFile)) {
        if ([string]::IsNullOrEmpty($line)) { continue }
        if ($line.Length -lt 67) { continue }
        $expected = $line.Substring(0, 64)
        $name = $line.Substring(66)
        if ([string]::IsNullOrEmpty($name)) { continue }

        $path = Join-Path $incoming $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            [Console]::Error.WriteLine("missing transferred file: $name")
            $failures++
            continue
        }
        $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected.ToLowerInvariant()) {
            [Console]::Error.WriteLine("SHA-256 mismatch for ${name}: host recorded $expected, guest computed $actual")
            $failures++
        }
    }
    if ($failures -ne 0) {
        [Console]::Error.WriteLine("$failures transferred file(s) do not match the host's SHA-256 digests; nothing was extracted")
        exit 1
    }
}

# --- 3. optional guest-side extraction --------------------------------------
#
# Extraction happens only in the guest, and only after every entry name has
# passed Test-ArchiveEntry and the archive has been confirmed link-free. A
# refused archive keeps its original file and creates no destination
# directory at all.

$refusalsFile = Join-Path $notes 'archive-refusals.txt'

function Write-Refusal {
    param([string] $Archive, [string] $Reason)
    $name = Split-Path -Leaf $Archive
    [Console]::Error.WriteLine("refusing to extract ${name}: $Reason")
    Add-Content -LiteralPath $refusalsFile -Value "${name}: $Reason"
}

# Returns $null when every entry of a zip is safe, or the reason it is not.
function Test-ZipArchive {
    param([string] $Path)

    try {
        $zip = [System.IO.Compression.ZipFile]::OpenRead($Path)
    } catch {
        return 'the archive could not be listed (is it a valid zip?)'
    }
    try {
        foreach ($entry in $zip.Entries) {
            # Unix mode lives in the upper 16 bits of the external
            # attributes; 0xA000 marks a symbolic link, which is never
            # extracted. Older runtimes without the property simply skip
            # this check -- the name rules below still apply.
            $attributes = $null
            try { $attributes = $entry.ExternalAttributes } catch { $attributes = $null }
            if ($null -ne $attributes) {
                $mode = ([int]$attributes -shr 16) -band 0xF000
                if ($mode -eq 0xA000) {
                    return 'it contains a symbolic link entry, which is never extracted'
                }
            }
            $reason = Test-ArchiveEntry -Name $entry.FullName
            if ($null -ne $reason) {
                return "entry `"$($entry.FullName)`": $reason"
            }
        }
    } finally {
        $zip.Dispose()
    }
    return $null
}

# Returns $null when every entry of a tar is safe, or the reason it is not.
# Uses tar.exe (bsdtar), which stock Windows 10 1803+ ships.
function Test-TarArchive {
    param([string] $Path)

    if ($null -eq (Get-Command 'tar.exe' -ErrorAction SilentlyContinue)) {
        return 'no tar.exe in this guest, so its entries cannot be inspected'
    }
    $verbose = & tar.exe -tvf $Path 2>$null
    if ($LASTEXITCODE -ne 0) {
        return 'the archive could not be listed (is it a valid tar?)'
    }
    foreach ($line in @($verbose)) {
        # tar's verbose listing marks symbolic links "l" and hard links "h"
        # in the first column of the mode string.
        if ($line -is [string] -and $line.Length -gt 0 -and ('l', 'h' -contains $line.Substring(0, 1))) {
            return 'it contains a symbolic or hard link entry, which is never extracted'
        }
    }
    $names = & tar.exe -tf $Path 2>$null
    if ($LASTEXITCODE -ne 0) {
        return 'the archive could not be listed (is it a valid tar?)'
    }
    foreach ($name in @($names)) {
        if ([string]::IsNullOrEmpty($name)) { continue }
        $reason = Test-ArchiveEntry -Name $name
        if ($null -ne $reason) {
            return "entry `"$name`": $reason"
        }
    }
    return $null
}

function Expand-ZipArchive {
    param([string] $Path, [string] $Destination)

    $destinationRoot = [System.IO.Path]::GetFullPath($Destination)
    if (-not $destinationRoot.EndsWith('\')) {
        $destinationRoot = $destinationRoot + '\'
    }
    $zip = [System.IO.Compression.ZipFile]::OpenRead($Path)
    try {
        foreach ($entry in $zip.Entries) {
            $relative = $entry.FullName.Replace('/', '\')
            $target = [System.IO.Path]::GetFullPath((Join-Path $Destination $relative))
            # Defence in depth: the entry names were validated above, so this
            # can only fire if a rule is ever loosened.
            if (-not $target.StartsWith($destinationRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
                throw "archive entry `"$($entry.FullName)`" resolves outside $Destination"
            }
            if ([string]::IsNullOrEmpty($entry.Name)) {
                New-Item -ItemType Directory -Force -Path $target | Out-Null
                continue
            }
            $parent = Split-Path -Parent $target
            if (-not [string]::IsNullOrEmpty($parent)) {
                New-Item -ItemType Directory -Force -Path $parent | Out-Null
            }
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $target, $true)
        }
    } finally {
        $zip.Dispose()
    }
}

if ($Extract -eq 'true') {
    try {
        Add-Type -AssemblyName System.IO.Compression.FileSystem -ErrorAction SilentlyContinue
    } catch {
        # .NET Core already has it loaded; nothing to do.
    }

    foreach ($archive in Get-ChildItem -LiteralPath $incoming -File -ErrorAction SilentlyContinue) {
        $lower = $archive.Name.ToLowerInvariant()
        $kind = $null
        $stem = $null
        if ($lower.EndsWith('.zip')) {
            $kind = 'zip'
            $stem = $archive.Name.Substring(0, $archive.Name.Length - 4)
        } elseif ($lower.EndsWith('.tar.gz')) {
            $kind = 'tar'
            $stem = $archive.Name.Substring(0, $archive.Name.Length - 7)
        } elseif ($lower.EndsWith('.tgz')) {
            $kind = 'tar'
            $stem = $archive.Name.Substring(0, $archive.Name.Length - 4)
        } elseif ($lower.EndsWith('.tar')) {
            $kind = 'tar'
            $stem = $archive.Name.Substring(0, $archive.Name.Length - 4)
        }
        if ($null -eq $kind) { continue }

        if ($kind -eq 'zip') {
            $reason = Test-ZipArchive -Path $archive.FullName
        } else {
            $reason = Test-TarArchive -Path $archive.FullName
        }
        if ($null -ne $reason) {
            Write-Refusal -Archive $archive.FullName -Reason $reason
            continue
        }

        $destination = Join-Path (Join-Path $incoming 'extracted') $stem
        New-Item -ItemType Directory -Force -Path $destination | Out-Null
        if ($kind -eq 'zip') {
            Expand-ZipArchive -Path $archive.FullName -Destination $destination
        } else {
            & tar.exe -xf $archive.FullName -C $destination
            if ($LASTEXITCODE -ne 0) {
                [Console]::Error.WriteLine("failed to extract $($archive.Name) (exit $LASTEXITCODE)")
                exit $LASTEXITCODE
            }
        }
    }
}

# --- 4. metadata summary -----------------------------------------------------

$metadataFile = Join-Path $incoming '.wargame.json'
$readme = Join-Path $notes 'README.md'
$lines = New-Object System.Collections.Generic.List[string]
$lines.Add('# Challenge notes')
$lines.Add('')

if (Test-Path -LiteralPath $metadataFile) {
    # Data, never code: ConvertFrom-Json parses, it does not evaluate.
    $metadata = Get-Content -LiteralPath $metadataFile -Raw -Encoding UTF8 | ConvertFrom-Json
    $provider = if ($metadata.provider) { $metadata.provider } else { 'unknown' }
    $challengeId = if ($metadata.challenge_id) { $metadata.challenge_id } else { 'unknown' }
    $title = if ($metadata.title) { $metadata.title } else { 'unknown' }
    $lines.Add("- provider: $provider")
    $lines.Add("- challenge id: $challengeId")
    $lines.Add("- title: $title")
    if ($metadata.download_sha256) {
        $lines.Add('- files:')
        foreach ($property in $metadata.download_sha256.PSObject.Properties) {
            $lines.Add("  - $($property.Name): $($property.Value)")
        }
    }
    $lines.Add('')
    $lines.Add('Downloaded files were NOT executed automatically. Review `incoming\` before running anything.')
} else {
    $lines.Add('No metadata file was found at incoming\.wargame.json.')
}
$lines.Add('')
[System.IO.File]::WriteAllLines($readme, $lines)

# --- 5. welcome banner --------------------------------------------------------

Write-Output ''
Write-Output '=============================================================='
Write-Output ' wargame-vm: workspace ready'
Write-Output '=============================================================='
Write-Output " Workspace : $Workspace"
Write-Output " Incoming  : $incoming"
Write-Output " Work      : $work"
Write-Output " Notes     : $readme"
Write-Output " Output    : $output"
Write-Output ''
Write-Output ' Downloaded challenge files were NOT executed automatically.'
Write-Output ' Review incoming\ before running anything.'
Write-Output '=============================================================='
Write-Output ''

if (Test-Path -LiteralPath $refusalsFile) {
    Write-Output " Some archives were left packed; see $refusalsFile"
    Write-Output ''
}

exit 0
