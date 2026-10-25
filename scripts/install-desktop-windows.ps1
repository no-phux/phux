#Requires -Version 5.1
<#
.SYNOPSIS
Install the signed native Windows desktop per user. The coordinator remains in WSL.
.EXAMPLE
& ([scriptblock]::Create((Invoke-WebRequest -UseBasicParsing https://raw.githubusercontent.com/no-phux/phux/main/scripts/install-desktop-windows.ps1).Content))
.EXAMPLE
.\install-desktop-windows.ps1 -Channel next
#>
[CmdletBinding()]
param(
    [ValidateSet('stable', 'next')][string] $Channel = 'stable',
    [ValidatePattern('^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$')][string] $Version,
    [string] $InstallDir = (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Phux'),
    [switch] $Uninstall,
    [switch] $NoShortcut
)
$ErrorActionPreference = 'Stop'
$DesktopReleaseKeys = @(
    'RWTJVmT0UFkQn2Zw1mG2cIcm5O/9kLuffefLpHw3vwhWAxgn/gcsoxiX',
    'RWRq3CeWWTfkuYUoONcL0LsC/L3BYav/SIeHRPDMauxHAZnrUd+UoYU1'
)

function ConvertTo-DesktopShellLiteral([string] $Value) {
    return "'" + $Value.Replace("'", "'\''") + "'"
}

function Invoke-DesktopWsl([string] $Script) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Script))
    $arguments = @()
    if ($env:PHUX_WSL_DISTRO) { $arguments += @('--distribution', $env:PHUX_WSL_DISTRO) }
    $arguments += @('--exec', 'sh', '-c', ('IFS=;set -f;eval $(printf %s ' + $encoded + '|base64 -d)'))
    $output = & wsl.exe @arguments
    if ($LASTEXITCODE -ne 0) { throw 'WSL verification failed. Install minisign in your WSL distribution (sudo apt-get install minisign), or put native minisign.exe on PATH.' }
    return ($output -join "`n")
}

function Test-DesktopSignature([string] $Sidecar, [string] $Signature, [string[]] $Keys = $DesktopReleaseKeys) {
    $lines = [IO.File]::ReadAllLines($Signature)
    if ($lines.Length -lt 2) { throw 'Malformed release signature' }
    $bytes = [Convert]::FromBase64String($lines[1])
    if ($bytes.Length -lt 2 -or [Text.Encoding]::ASCII.GetString($bytes, 0, 2) -cne 'ED') {
        throw 'Release signatures must use minisign prehash mode'
    }
    $native = Get-Command minisign -CommandType Application -ErrorAction SilentlyContinue
    foreach ($key in $Keys) {
        if ($native) {
            $preference = $ErrorActionPreference
            try {
                $ErrorActionPreference = 'Continue'
                & $native.Source -Vm $Sidecar -x $Signature -P $key -H -q 2>$null | Out-Null
                $verified = $LASTEXITCODE -eq 0
            } finally { $ErrorActionPreference = $preference }
            if ($verified) { return }
        } else {
            $message = ConvertTo-DesktopShellLiteral $Sidecar
            $sig = ConvertTo-DesktopShellLiteral $Signature
            $script = 'command -v minisign >/dev/null || exit 127; minisign -Vm "$(wslpath -a -u ' + $message + ')" -x "$(wslpath -a -u ' + $sig + ')" -P ' + (ConvertTo-DesktopShellLiteral $key) + ' -H -q >/dev/null 2>&1 && printf VERIFIED || printf REFUSED'
            if ((Invoke-DesktopWsl $script) -ceq 'VERIFIED') { return }
        }
    }
    throw 'The release checksum is not signed by a trusted phux release key. Nothing was installed.'
}

function Get-DesktopSignedDigest([string] $Sidecar, [string] $Signature, [string] $Archive) {
    Test-DesktopSignature $Sidecar $Signature
    $fields = ([IO.File]::ReadAllText($Sidecar).Trim() -split '\s+')
    if ($fields.Count -ne 2 -or $fields[0] -cnotmatch '^[0-9a-f]{64}$' -or $fields[1] -cne $Archive) {
        throw 'The signed checksum must name exactly the requested archive'
    }
    return $fields[0]
}

function Expand-DesktopArchive([string] $Archive, [string] $Destination) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        [long] $total = 0
        if ($zip.Entries.Count -gt 20000) { throw 'Desktop ZIP contains too many entries' }
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace('\', '/')
            if ($name -cnotmatch '^phux-desktop/' -or $name -match '(^|/)\.\.(/|$)|:|^/' -or -not $seen.Add($name)) { throw "Unsafe ZIP entry: $name" }
            if ((($entry.ExternalAttributes -shr 16) -band 0xF000) -eq 0xA000) { throw 'ZIP symlinks are not allowed' }
            $total += $entry.Length
            if ($total -gt 1GB) { throw 'Desktop ZIP exceeds its extraction limit' }
        }
    } finally { $zip.Dispose() }
    [IO.Compression.ZipFile]::ExtractToDirectory($Archive, $Destination)
    return Join-Path $Destination 'phux-desktop'
}

function Get-DesktopBundle([string] $Bundle) {
    $release = Get-Content -Raw (Join-Path $Bundle 'share/phux-desktop/release.json') | ConvertFrom-Json
    if ($release.identifier -cne 'dev.phux.desktop' -or $release.target -cne 'x86_64-pc-windows-msvc' -or $release.sha -cnotmatch '^[0-9a-f]{40}$') { throw 'Invalid desktop bundle identity' }
    if ($release.version -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' -or $release.channel -cnotmatch '^(stable|next)$') { throw 'Invalid desktop release identity' }
    foreach ($name in @('phux-desktop.exe', 'vcruntime140.dll', 'msvcp140.dll', 'share/phux-desktop/LICENSE', 'share/phux-desktop/NOTICE', 'share/phux-desktop/THIRD-PARTY-NOTICES.txt')) {
        if (-not (Test-Path -LiteralPath (Join-Path $Bundle $name) -PathType Leaf)) { throw "Desktop bundle is missing $name" }
    }
    return $release
}

function Assert-DesktopStopped([string] $Directory) {
    $prefix = [IO.Path]::GetFullPath($Directory).TrimEnd('\') + '\'
    foreach ($process in @(Get-Process phux-desktop -ErrorAction SilentlyContinue)) {
        if ($process.Path -and $process.Path.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Close this Phux desktop before installing or uninstalling. Its WSL shells will survive.' }
    }
}

function Set-DesktopRegistration([string] $Directory, [string] $ReleaseVersion, [bool] $Shortcut) {
    $registry = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\PhuxDesktop'
    New-Item -Force $registry | Out-Null
    $uninstaller = Join-Path $Directory 'uninstall.ps1'
    $command = 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "' + $uninstaller + '" -Uninstall -InstallDir "' + $Directory + '"'
    foreach ($item in @{ DisplayName = 'Phux Desktop'; DisplayVersion = $ReleaseVersion; Publisher = 'no-phux'; InstallLocation = $Directory; UninstallString = $command; QuietUninstallString = $command }.GetEnumerator()) {
        Set-ItemProperty $registry -Name $item.Key -Value $item.Value
    }
    if ($Shortcut) {
        $path = Join-Path ([Environment]::GetFolderPath('Programs')) 'Phux Desktop.lnk'
        $link = (New-Object -ComObject WScript.Shell).CreateShortcut($path)
        $link.TargetPath = Join-Path $Directory 'current/phux-desktop.exe'
        $link.WorkingDirectory = Join-Path $Directory 'current'
        $link.Save()
    }
}

# Internal transaction used by native CI to qualify its locally built bytes.
# The public installer reaches this only AFTER signature, digest and identity checks.
function Install-DesktopBundle([string] $Bundle, [string] $Directory, [string] $Installer, [bool] $Shortcut = $true) {
    $release = Get-DesktopBundle $Bundle
    Assert-DesktopStopped $Directory
    New-Item -ItemType Directory -Force $Directory | Out-Null
    $lock = [IO.File]::Open((Join-Path $Directory '.install.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
    $current = Join-Path $Directory 'current'
    $stage = Join-Path $Directory ('.stage-' + [Guid]::NewGuid().ToString('N'))
    $backup = Join-Path $Directory ('.previous-' + [Guid]::NewGuid().ToString('N'))
    try {
        if (Test-Path $current) { Get-DesktopBundle $current | Out-Null }
        Copy-Item -LiteralPath $Bundle -Destination $stage -Recurse
        if (Test-Path $current) { Move-Item -LiteralPath $current -Destination $backup }
        try {
            Move-Item -LiteralPath $stage -Destination $current
            Copy-Item -LiteralPath $Installer -Destination (Join-Path $Directory 'uninstall.ps1') -Force
            Set-DesktopRegistration $Directory $release.version $Shortcut
        } catch {
            if (Test-Path $current) { Remove-Item -LiteralPath $current -Recurse -Force }
            if (Test-Path $backup) { Move-Item -LiteralPath $backup -Destination $current }
            throw
        }
        if (Test-Path $backup) { Remove-Item -LiteralPath $backup -Recurse -Force }
    } finally {
        if (Test-Path $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
        $lock.Dispose()
    }
    Write-Output "Installed native Phux Desktop $($release.version) in $current. The WSL coordinator was not modified."
}

function Uninstall-Desktop([string] $Directory) {
    $current = Join-Path $Directory 'current'
    Get-DesktopBundle $current | Out-Null
    Assert-DesktopStopped $Directory
    $lock = [IO.File]::Open((Join-Path $Directory '.install.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
    try {
        Remove-Item -LiteralPath $current -Recurse -Force
        $registry = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\PhuxDesktop'
        $recorded = Get-ItemProperty $registry -ErrorAction SilentlyContinue
        if ($recorded.InstallLocation -eq $Directory) {
            Remove-Item $registry -Recurse -Force
            $shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Phux Desktop.lnk'
            if (Test-Path $shortcut) { Remove-Item -LiteralPath $shortcut -Force }
        }
        Remove-Item -LiteralPath (Join-Path $Directory 'uninstall.ps1') -Force -ErrorAction SilentlyContinue
    } finally { $lock.Dispose() }
    Write-Output 'Uninstalled the Windows app. Preferences and WSL shells were preserved.'
}

function Get-DesktopReleaseSelection([string] $ReleaseChannel, [string] $RequestedVersion, [string] $Work) {
    if ($RequestedVersion -and $ReleaseChannel -ne 'stable') { throw '-Version is only valid with -Channel stable' }
    $manifest = $null
    $selectedVersion = $RequestedVersion
    if (-not $selectedVersion) {
        $url = "https://github.com/no-phux/phux/releases/download/channels/$ReleaseChannel.json"
        $manifestFile = Join-Path $Work 'channel.json'
        Invoke-WebRequest -UseBasicParsing $url -OutFile $manifestFile
        $document = Get-Content -Raw $manifestFile | ConvertFrom-Json
        if ($document.schema_version -ne 1 -or $document.channel -cne $ReleaseChannel) { throw 'Unsupported desktop channel manifest' }
        $manifest = $document.products.desktop
        if ($manifest.version -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' -or $manifest.sha -cnotmatch '^[0-9a-f]{40}$') { throw 'Invalid desktop channel manifest' }
        if ($manifest.artifacts.'x86_64-pc-windows-msvc'.sha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'This channel has no native Windows desktop release' }
        $selectedVersion = $manifest.version
    }
    $identity = $selectedVersion
    $tag = "desktop-v$selectedVersion"
    if ($ReleaseChannel -eq 'next') { $identity = 'next.' + $manifest.sha; $tag = 'next' }
    return @{
        Version = $selectedVersion; Channel = $ReleaseChannel; Sha = $manifest.sha
        Digest = $manifest.artifacts.'x86_64-pc-windows-msvc'.sha256
        Archive = "phux-desktop-$identity-windows-x64.zip"
        Base = "https://github.com/no-phux/phux/releases/download/$tag"
    }
}

function Get-VerifiedDesktopDownload([hashtable] $Release, [string] $Work) {
    $archive = $Release.Archive
    $base = $Release.Base
    foreach ($suffix in @('.sha256', '.sha256.minisig')) {
        Invoke-WebRequest -UseBasicParsing "$base/$archive$suffix" -OutFile (Join-Path $Work ($archive + $suffix))
    }
    $digest = Get-DesktopSignedDigest (Join-Path $Work "$archive.sha256") (Join-Path $Work "$archive.sha256.minisig") $archive
    if ($Release.Digest -and $Release.Digest -cne $digest) { throw 'Manifest and signed desktop digest disagree' }
    $zip = Join-Path $Work $archive
    Invoke-WebRequest -UseBasicParsing "$base/$archive" -OutFile $zip
    if ((Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant() -cne $digest) { throw 'Desktop archive checksum mismatch' }
    $bundle = Expand-DesktopArchive $zip (Join-Path $Work 'unpacked')
    $metadata = Get-DesktopBundle $bundle
    if ($metadata.version -cne $Release.Version -or $metadata.channel -cne $Release.Channel) { throw 'Desktop bundle version/channel mismatch' }
    if ($Release.Sha -and $metadata.sha -cne $Release.Sha) { throw 'Desktop bundle revision mismatch' }
    return $bundle
}

function Install-DesktopRelease {
    if ($env:OS -ne 'Windows_NT' -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw 'This desktop installer requires native x64 Windows' }
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $work = Join-Path ([IO.Path]::GetTempPath()) ('phux-desktop-install-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory $work | Out-Null
    try {
        $selected = Get-DesktopReleaseSelection $Channel $Version $work
        $bundle = Get-VerifiedDesktopDownload $selected $work
        Install-DesktopBundle $bundle ([IO.Path]::GetFullPath($InstallDir)) (Join-Path $bundle 'install.ps1') (-not $NoShortcut)
    } finally { Remove-Item -LiteralPath $work -Recurse -Force }
}

if ($MyInvocation.InvocationName -ne '.') {
    if ($Uninstall) { Uninstall-Desktop ([IO.Path]::GetFullPath($InstallDir)) } else { Install-DesktopRelease }
}
