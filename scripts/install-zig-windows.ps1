#Requires -Version 5.1
# Install only the audited compiler pinned by the selected source checkout.
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string] $Directory)
$ErrorActionPreference = 'Stop'
$pin = Get-Content -Raw (Join-Path $PSScriptRoot '../.config/zig-toolchain.json') | ConvertFrom-Json
$archive = $pin.archives.'x86_64-windows'
if (-not $archive -or $archive.sha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'Missing audited Windows Zig pin' }
$destination = Join-Path $Directory ([IO.Path]::GetFileNameWithoutExtension($archive.archive))
$compiler = Join-Path $destination 'zig.exe'
if (Test-Path $destination) {
    if (-not (Test-Path $compiler) -or (& $compiler version) -cne $pin.version) {
        throw "Existing Zig directory is invalid; choose another directory: $destination"
    }
    Write-Output $destination
    return
}
New-Item -ItemType Directory -Force $Directory | Out-Null
$stage = Join-Path $Directory ('.zig-download-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $stage | Out-Null
try {
    $zip = Join-Path $stage $archive.archive
    Invoke-WebRequest -UseBasicParsing -Uri "https://ziglang.org/download/$($pin.version)/$($archive.archive)" -OutFile $zip
    if ((Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant() -cne $archive.sha256) { throw 'Zig checksum mismatch' }
    Expand-Archive -LiteralPath $zip -DestinationPath $stage
    $unpacked = Join-Path $stage ([IO.Path]::GetFileNameWithoutExtension($archive.archive))
    if ((& (Join-Path $unpacked 'zig.exe') version) -cne $pin.version) { throw 'Zig version mismatch' }
    Move-Item -LiteralPath $unpacked -Destination $destination
    Write-Output $destination
} finally {
    Remove-Item -LiteralPath $stage -Recurse -Force
}
