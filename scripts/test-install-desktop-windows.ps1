#Requires -Version 5.1
# Portable trust/archive/transaction contracts; Windows CI runs both PS hosts.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'install-desktop-windows.ps1')

function Assert-DesktopTest([bool] $Condition, [string] $Message) {
    if (-not $Condition) { throw $Message }
}
function Assert-DesktopRefused([scriptblock] $Action, [string] $ExpectedMessage) {
    $failure = $null
    try { & $Action | Out-Null } catch { $failure = $_ }
    Assert-DesktopTest ($null -ne $failure) 'Unsafe input was not refused'
    if ($ExpectedMessage) { Assert-DesktopTest ($failure.Exception.Message -ceq $ExpectedMessage) "Unexpected refusal: $($failure.Exception.Message)" }
}

# Keep native registry/COM coverage on Windows without touching a real install.
# Portable runs substitute only those OS boundaries; the transaction is shared.
$script:nativeWindows = $env:OS -eq 'Windows_NT'
$script:realSetDesktopRegistration = ${function:Set-DesktopRegistration}
$script:testRegistryPath = 'HKCU:\Software\PhuxDesktopInstallerTest-' + [Guid]::NewGuid().ToString('N')
function Get-DesktopRegistrationPath { return $script:testRegistryPath }
function Get-DesktopShortcutPath { return $script:testShortcutPath }
if (-not $script:nativeWindows) {
    $script:portableRegistry = @{ Exists = $false; Values = @{} }
    function Get-DesktopRegistrySnapshot {
        $values = @{}
        foreach ($item in $script:portableRegistry.Values.GetEnumerator()) { $values[$item.Key] = $item.Value.Clone() }
        return @{ Exists = $script:portableRegistry.Exists; Values = $values }
    }
    function Restore-DesktopRegistrySnapshot([hashtable] $State) { $script:portableRegistry = $State }
}

function Set-DesktopTestRegistryValue([string] $Name, $Value, [string] $Kind = 'String') {
    if ($script:nativeWindows) {
        if (-not (Test-Path -LiteralPath (Get-DesktopRegistrationPath))) { New-Item -Force (Get-DesktopRegistrationPath) | Out-Null }
        $key = Open-DesktopWritableRegistryKey (Get-DesktopRegistrationPath)
        try { $key.SetValue($Name, $Value, [Microsoft.Win32.RegistryValueKind] $Kind) } finally { $key.Dispose() }
        return
    }
    $script:portableRegistry.Exists = $true
    $script:portableRegistry.Values[$Name] = @{ Value = $Value; Kind = [Microsoft.Win32.RegistryValueKind] $Kind }
}

function Set-DesktopRegistration([string] $Directory, [string] $ReleaseVersion, [bool] $Shortcut) {
    if ($script:failRegistration -eq 'partial-registry') {
        Set-DesktopTestRegistryValue 'DisplayVersion' 'partially written'
        Set-DesktopTestRegistryValue 'NewPartialValue' 42 'DWord'
        Set-DesktopTestRegistryValue 'CustomExpand' '%PARTIALLY_WRITTEN%' 'String'
        throw 'Injected failure after partial registry writes'
    }
    if ($script:nativeWindows) {
        & $script:realSetDesktopRegistration $Directory $ReleaseVersion $Shortcut
    } else {
        Set-DesktopTestRegistryValue 'DisplayVersion' $ReleaseVersion
        Set-DesktopTestRegistryValue 'InstallLocation' $Directory
        if ($Shortcut) { [IO.File]::WriteAllText((Get-DesktopShortcutPath), "shortcut to $Directory") }
    }
    if ($script:failRegistration -eq 'shortcut') {
        Assert-DesktopTest (Test-Path -LiteralPath (Get-DesktopShortcutPath)) 'Shortcut failure was not injected after creation'
        throw 'Injected failure after shortcut creation'
    }
}

function Get-DesktopTestIdentity([string] $Directory) {
    $parts = @()
    if (Test-Path -LiteralPath $Directory) {
        foreach ($file in @(Get-ChildItem -LiteralPath $Directory -File -Recurse | Sort-Object FullName)) {
            # A lock is transaction coordination, not installed payload.
            if ($file.Name -eq '.install.lock') { continue }
            $parts += $file.FullName.Substring($Directory.Length) + ':' + (Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash
        }
    }
    $shortcut = Get-DesktopShortcutPath
    if (Test-Path -LiteralPath $shortcut) { $parts += 'shortcut:' + (Get-FileHash -Algorithm SHA256 -LiteralPath $shortcut).Hash }
    $registry = Get-DesktopRegistrySnapshot
    $parts += 'registry:' + $registry.Exists
    foreach ($name in @($registry.Values.Keys | Sort-Object)) {
        $entry = $registry.Values[$name]
        $parts += $name + ':' + $entry.Kind + ':' + (ConvertTo-Json -InputObject $entry.Value -Compress)
    }
    return $parts -join "`n"
}

function Assert-DesktopFailedInstall([string] $Bundle, [string] $Directory, [string] $Installer, [string] $Failure, [bool] $Shortcut = $true) {
    $before = Get-DesktopTestIdentity $Directory
    $script:failRegistration = $Failure
    $expected = @{ 'partial-registry' = 'Injected failure after partial registry writes'; shortcut = 'Injected failure after shortcut creation' }
    try { Assert-DesktopRefused { Install-DesktopBundle $Bundle $Directory $Installer $Shortcut } $expected[$Failure] }
    finally { $script:failRegistration = $null }
    Assert-DesktopTest ((Get-DesktopTestIdentity $Directory) -ceq $before) "$Failure did not restore exact file bytes and registry values/types"
    Assert-DesktopTest (@(Get-ChildItem -LiteralPath $Directory -Force | Where-Object { $_.Name -match '^\.(stage|previous)-' }).Count -eq 0) 'Rollback left transaction payloads behind'
}

function Invoke-DesktopFixtureMinisign([string] $Message, [string] $PublicKey, [string] $SecretKey) {
    $arguments = @('-S', '-s', $SecretKey, '-m', $Message)
    if (-not (Test-Path -LiteralPath $SecretKey)) { $arguments = @('-G', '-W', '-p', $PublicKey, '-s', $SecretKey) }
    $native = Get-Command minisign -CommandType Application -ErrorAction SilentlyContinue
    if ($native) {
        & $native.Source @arguments | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Could not sign desktop test fixture' }
        return
    }
    $shell = @()
    foreach ($argument in $arguments) {
        if ($argument.StartsWith('-')) { $shell += $argument; continue }
        $shell += '"$(wslpath -a -u ' + (ConvertTo-DesktopShellLiteral $argument) + ')"'
    }
    Invoke-DesktopWsl ('minisign ' + ($shell -join ' ')) | Out-Null
}

function Invoke-WebRequest([switch] $UseBasicParsing, [string] $Uri, [string] $OutFile) {
    $script:fixtureRequests += $Uri
    if (-not $script:fixtureDownloads.ContainsKey($Uri)) { throw "Unexpected desktop download: $Uri" }
    Copy-Item -LiteralPath $script:fixtureDownloads[$Uri] -Destination $OutFile
}

function Install-DesktopFixtureDownload([hashtable] $Selection, [string] $Directory) {
    $download = Join-Path $work ([Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory $download | Out-Null
    $verified = Get-VerifiedDesktopDownload $Selection $download
    Install-DesktopBundle $verified $Directory (Join-Path $verified 'install.ps1') $true
}

$work = Join-Path ([IO.Path]::GetTempPath()) ('phux-desktop-contract-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $work | Out-Null
$script:testShortcutPath = Join-Path $work 'Phux Desktop.lnk'
$productionKeys = $DesktopReleaseKeys
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
    $installer = Join-Path $bundle 'install.ps1'
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'install-desktop-windows.ps1') -Destination $installer
    $validZip = Join-Path $work 'valid.zip'
    [IO.Compression.ZipFile]::CreateFromDirectory($bundle, $validZip, 'Optimal', $true)
    $expanded = Expand-DesktopArchive $validZip (Join-Path $work 'valid-expanded')
    Assert-DesktopTest ((Get-DesktopBundle $expanded).sha -ceq ('a' * 40)) 'A valid portable ZIP did not round trip'
    $directory = Join-Path $work 'installed with spaces'
    foreach ($failure in @('partial-registry', 'shortcut')) {
        Assert-DesktopFailedInstall $bundle $directory $installer $failure
    }
    Assert-DesktopFailedInstall $bundle $directory $installer 'partial-registry' $false

    # Serve local bytes, but perform real minisign, SHA256, ZIP and identity checks.
    $publicKey = Join-Path $work 'fixture.pub'
    $secretKey = Join-Path $work 'fixture.key'
    Invoke-DesktopFixtureMinisign $null $publicKey $secretKey
    $DesktopReleaseKeys = @([IO.File]::ReadAllLines($publicKey)[1])
    $downloadSidecar = Join-Path $work ($selected.Archive + '.sha256')
    $digest = (Get-FileHash -Algorithm SHA256 -LiteralPath $validZip).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText($downloadSidecar, "$digest  $($selected.Archive)`n", [Text.UTF8Encoding]::new($false))
    Invoke-DesktopFixtureMinisign $downloadSidecar $publicKey $secretKey
    $selected.Sha = $release.sha
    $selected.Digest = $digest
    $script:fixtureRequests = @()
    $script:fixtureDownloads = @{
        "$($selected.Base)/$($selected.Archive)" = $validZip
        "$($selected.Base)/$($selected.Archive).sha256" = $downloadSidecar
        "$($selected.Base)/$($selected.Archive).sha256.minisig" = ($downloadSidecar + '.minisig')
    }
    Install-DesktopFixtureDownload $selected $directory
    Assert-DesktopTest ((Get-DesktopBundle (Join-Path $directory 'current')).version -ceq '1.2.3') 'Verified signed install failed'
    Assert-DesktopTest ((Get-DesktopRegistrySnapshot).Values.DisplayVersion.Value -ceq '1.2.3') 'Signed install did not register its release'
    Assert-DesktopTest ((Get-FileHash -LiteralPath (Join-Path $directory 'uninstall.ps1')).Hash -ceq (Get-FileHash -LiteralPath $installer).Hash) 'Signed install did not install its uninstaller'
    Assert-DesktopTest (Test-Path -LiteralPath (Get-DesktopShortcutPath)) 'Signed install did not create a shortcut'

    $before = Get-DesktopTestIdentity $directory
    $mismatches = @(
        @{ Field = 'Digest'; Value = ('0' * 64); Error = 'Manifest and signed desktop digest disagree' },
        @{ Field = 'Sha'; Value = ('b' * 40); Error = 'Desktop bundle revision mismatch' },
        @{ Field = 'Channel'; Value = 'next'; Error = 'Desktop bundle version/channel mismatch' },
        @{ Field = 'Version'; Value = '1.2.4'; Error = 'Desktop bundle version/channel mismatch' }
    )
    foreach ($mismatch in $mismatches) {
        $invalid = $selected.Clone()
        $invalid[$mismatch.Field] = $mismatch.Value
        $script:fixtureRequests = @()
        Assert-DesktopRefused { Install-DesktopFixtureDownload $invalid $directory } $mismatch.Error
        Assert-DesktopTest ((Get-DesktopTestIdentity $directory) -ceq $before) 'Rejected download modified installed files or registration'
        if ($mismatch.Field -eq 'Digest') {
            Assert-DesktopTest ($script:fixtureRequests.Count -eq 2) 'Manifest digest mismatch downloaded the untrusted archive'
        }
    }
    $tamperedZip = Join-Path $work 'tampered.zip'
    Copy-Item -LiteralPath $validZip -Destination $tamperedZip
    [IO.File]::AppendAllText($tamperedZip, 'tamper')
    $script:fixtureDownloads["$($selected.Base)/$($selected.Archive)"] = $tamperedZip
    Assert-DesktopRefused { Install-DesktopFixtureDownload $selected $directory } 'Desktop archive checksum mismatch'
    Assert-DesktopTest ((Get-DesktopTestIdentity $directory) -ceq $before) 'Archive checksum rejection modified the installation'

    # An upgrade must restore bytes, absent values, and non-string registry kinds.
    [IO.File]::WriteAllText((Join-Path $directory 'keep.txt'), 'unrelated state')
    [IO.File]::WriteAllText((Join-Path $directory 'uninstall.ps1'), 'original uninstaller bytes')
    if ($script:nativeWindows) {
        $oldLink = (New-Object -ComObject WScript.Shell).CreateShortcut((Get-DesktopShortcutPath))
        $oldLink.TargetPath = Join-Path $directory 'keep.txt'
        $oldLink.WorkingDirectory = $work
        $oldLink.Save()
    } else { [IO.File]::WriteAllText((Get-DesktopShortcutPath), 'original shortcut bytes') }
    Set-DesktopTestRegistryValue 'DisplayVersion' 'original registration'
    Set-DesktopTestRegistryValue 'CustomExpand' '%ORIGINAL_VALUE%' 'ExpandString'
    Set-DesktopTestRegistryValue 'CustomBinary' ([byte[]] @(0, 127, 255)) 'Binary'
    Set-DesktopTestRegistryValue '' 'original default value'
    if ($script:nativeWindows) { Remove-ItemProperty -LiteralPath (Get-DesktopRegistrationPath) -Name Publisher }
    $release.version = '1.2.4'
    $release | ConvertTo-Json | Set-Content (Join-Path $share 'release.json') -Encoding UTF8
    foreach ($failure in @('partial-registry', 'shortcut')) {
        Assert-DesktopFailedInstall $bundle $directory $installer $failure
        Assert-DesktopTest ((Get-DesktopBundle (Join-Path $directory 'current')).version -ceq '1.2.3') 'Failed replacement did not roll back'
    }
    Assert-DesktopFailedInstall $bundle $directory $installer 'partial-registry' $false
    Assert-DesktopTest (Test-Path (Join-Path $directory 'keep.txt')) 'Install removed unrelated files'
    $oldShortcutHash = (Get-FileHash -LiteralPath (Get-DesktopShortcutPath)).Hash
    Install-DesktopBundle $bundle $directory $installer $false
    Assert-DesktopTest ((Get-DesktopBundle (Join-Path $directory 'current')).version -ceq '1.2.4') 'A successful upgrade did not replace current'
    $registered = Get-DesktopRegistrySnapshot
    Assert-DesktopTest ($registered.Values.DisplayVersion.Value -ceq '1.2.4') 'Successful upgrade registration kept the old release'
    Assert-DesktopTest ($registered.Values.CustomExpand.Value -ceq '%ORIGINAL_VALUE%') 'Successful upgrade removed an unrelated registry value'
    Assert-DesktopTest ($registered.Values.CustomExpand.Kind -eq [Microsoft.Win32.RegistryValueKind]::ExpandString) 'Successful upgrade changed an unrelated registry kind'
    Assert-DesktopTest ((Get-FileHash -LiteralPath (Get-DesktopShortcutPath)).Hash -ceq $oldShortcutHash) 'NoShortcut modified an existing shortcut'
    Assert-DesktopTest ((Get-FileHash -LiteralPath (Join-Path $directory 'uninstall.ps1')).Hash -ceq (Get-FileHash -LiteralPath $installer).Hash) 'Successful upgrade kept the old uninstaller'
    # A locked replacement must not hide the triggering failure, skip remaining
    # registration restoration, or destroy the previous recoverable payload.
    & {
        $state = Get-DesktopInstallSnapshot $directory $true
        $prior = Get-DesktopTestIdentity $directory
        [IO.File]::WriteAllText((Join-Path $directory 'uninstall.ps1'), 'partial uninstaller')
        [IO.File]::WriteAllText((Get-DesktopShortcutPath), 'partial shortcut')
        Set-DesktopTestRegistryValue 'DisplayVersion' 'partial registration'
        $current = Join-Path $work 'locked-replacement'
        $backup = Join-Path $work 'recoverable-previous'
        New-Item -ItemType Directory $current, $backup | Out-Null
        [IO.File]::WriteAllText((Join-Path $backup 'payload'), 'previous bytes')
        function Remove-Item {
            [CmdletBinding()]
            param([string] $LiteralPath, [switch] $Recurse, [switch] $Force)
            if ($LiteralPath -ceq $current) { throw 'Injected locked replacement' }
            Microsoft.PowerShell.Management\Remove-Item @PSBoundParameters
        }
        try { throw 'Injected registration failure' } catch { $cause = $_ }
        $failure = $null
        try { Undo-DesktopInstall $state $current $backup $cause } catch { $failure = $_.Exception.Message }
        Assert-DesktopTest ($failure.Contains('Injected registration failure') -and $failure.Contains('Injected locked replacement') -and $failure.Contains($backup)) 'Incomplete rollback did not report both failures and the recovery location'
        Assert-DesktopTest ((Get-DesktopTestIdentity $directory) -ceq $prior) 'Payload rollback failure skipped other registration restoration'
        Assert-DesktopTest ([IO.File]::ReadAllText((Join-Path $backup 'payload')) -ceq 'previous bytes') 'Incomplete rollback destroyed the previous payload'
    }
    Write-Output 'Windows desktop signed download, identity, signature rotation, tamper, traversal and full rollback contracts passed.'
} finally {
    $DesktopReleaseKeys = $productionKeys
    if ($script:nativeWindows -and (Test-Path -LiteralPath (Get-DesktopRegistrationPath))) { Remove-Item -LiteralPath (Get-DesktopRegistrationPath) -Recurse -Force }
    Remove-Item -LiteralPath $work -Recurse -Force
}
