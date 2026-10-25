#Requires -Version 5.1
# Portable trust/archive/transaction contracts; Windows CI runs both PS hosts.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'install-desktop-windows.ps1')

function Assert-DesktopTest([bool] $Condition, [string] $Message) {
    if (-not $Condition) { throw $Message }
}
function Assert-DesktopRefused([scriptblock] $Action) {
    $refused = $false
    try { & $Action | Out-Null } catch { $refused = $true }
    Assert-DesktopTest $refused 'Unsafe input was not refused'
}

$work = Join-Path ([IO.Path]::GetTempPath()) ('phux-desktop-contract-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $work | Out-Null
try {
    $selected = Get-DesktopReleaseSelection 'stable' '1.2.3' $work
    Assert-DesktopTest ($selected.Archive -ceq 'phux-desktop-1.2.3-windows-x64.zip') 'Pinned stable artifact name changed'
    Assert-DesktopRefused { Get-DesktopReleaseSelection 'next' '1.2.3' $work }
    # Real production-key signatures, not an unsigned acceptance flag or mock.
    $sidecar = Join-Path $work 'release.sha256'
    $archive = 'phux-v9.9.9-aarch64-apple-darwin.tar.gz'
    $text = ('0' * 64) + "  $archive`n"
    [IO.File]::WriteAllText($sidecar, $text, [Text.UTF8Encoding]::new($false))
    $data = Join-Path $PSScriptRoot '../crates/phux-client-runtime/src/release/testdata'
    foreach ($signature in @('release-key-check.sha256.minisig', 'next-key-check.sha256.minisig')) {
        $sig = Join-Path $data $signature
        Assert-DesktopTest ((Get-DesktopSignedDigest $sidecar $sig $archive) -ceq ('0' * 64)) 'A trusted key was rejected'
        Assert-DesktopRefused { Get-DesktopSignedDigest $sidecar $sig 'wrong.zip' }
    }
    [IO.File]::WriteAllText($sidecar, $text.Replace('0', '1'), [Text.UTF8Encoding]::new($false))
    Assert-DesktopRefused { Get-DesktopSignedDigest $sidecar (Join-Path $data 'release-key-check.sha256.minisig') $archive }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zipFile = Join-Path $work 'bad.zip'
    $zip = [IO.Compression.ZipFile]::Open($zipFile, 'Create')
    $zip.CreateEntry('phux-desktop/../../escape') | Out-Null
    $zip.Dispose()
    Assert-DesktopRefused { Expand-DesktopArchive $zipFile (Join-Path $work 'escaped') }
    Assert-DesktopTest (-not (Test-Path (Join-Path $work 'escape'))) 'ZIP traversal wrote outside staging'

    $bundle = Join-Path $work 'phux-desktop'
    $share = Join-Path $bundle 'share/phux-desktop'
    New-Item -ItemType Directory -Force $share | Out-Null
    foreach ($name in @('phux-desktop.exe', 'vcruntime140.dll', 'msvcp140.dll')) { [IO.File]::WriteAllText((Join-Path $bundle $name), 'fixture') }
    foreach ($name in @('LICENSE', 'NOTICE', 'THIRD-PARTY-NOTICES.txt')) { [IO.File]::WriteAllText((Join-Path $share $name), 'fixture') }
    $release = @{ identifier = 'dev.phux.desktop'; target = 'x86_64-pc-windows-msvc'; version = '1.2.3'; sha = ('a' * 40); channel = 'stable' }
    $release | ConvertTo-Json | Set-Content (Join-Path $share 'release.json') -Encoding UTF8
    $validZip = Join-Path $work 'valid.zip'
    [IO.Compression.ZipFile]::CreateFromDirectory($bundle, $validZip, 'Optimal', $true)
    $expanded = Expand-DesktopArchive $validZip (Join-Path $work 'valid-expanded')
    Assert-DesktopTest ((Get-DesktopBundle $expanded).sha -ceq ('a' * 40)) 'A valid portable ZIP did not round trip'
    $directory = Join-Path $work 'installed with spaces'
    # The transaction is real; registry and shortcut effects are covered by the
    # disposable Windows GUI qualification, never by this developer-machine test.
    function Set-DesktopRegistration([string] $Directory, [string] $ReleaseVersion, [bool] $Shortcut) {
        $script:observedVersion = $ReleaseVersion
        if ($script:failRegistration) { throw 'Injected registration failure' }
    }
    Install-DesktopBundle $bundle $directory (Join-Path $PSScriptRoot 'install-desktop-windows.ps1') $false
    Assert-DesktopTest ($observedVersion -ceq '1.2.3') 'Install registration did not carry the release version'
    [IO.File]::WriteAllText((Join-Path $directory 'keep.txt'), 'unrelated state')
    $release.version = '1.2.4'
    $release | ConvertTo-Json | Set-Content (Join-Path $share 'release.json') -Encoding UTF8
    $script:failRegistration = $true
    Assert-DesktopRefused { Install-DesktopBundle $bundle $directory (Join-Path $PSScriptRoot 'install-desktop-windows.ps1') $false }
    Assert-DesktopTest ((Get-DesktopBundle (Join-Path $directory 'current')).version -ceq '1.2.3') 'Failed replacement did not roll back'
    Assert-DesktopTest (Test-Path (Join-Path $directory 'keep.txt')) 'Install removed unrelated files'
    Write-Output 'Windows desktop signature, rotation, tamper, traversal and rollback contracts passed.'
} finally { Remove-Item -LiteralPath $work -Recurse -Force }
