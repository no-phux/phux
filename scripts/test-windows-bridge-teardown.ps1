#Requires -Version 5.1
# A real native wsl.exe pipe, not a Linux simulation of launcher teardown.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string] $ServerBinary,
    [Parameter(Mandatory = $true)][string] $Output
)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:OS -ne 'Windows_NT') { throw 'Bridge qualification requires a disposable native Windows CI runner' }
if ($env:PHUX_WSL_DISTRO -cnotmatch '^[A-Za-z0-9._-]+$') { throw 'Select the disposable CI WSL distribution with PHUX_WSL_DISTRO' }
. (Join-Path $PSScriptRoot 'install-desktop-windows.ps1')
$Output = [IO.Path]::GetFullPath($Output)
New-Item -ItemType Directory -Force $Output | Out-Null
$root = '/tmp/phux-bridge-ci-' + [Guid]::NewGuid().ToString('N')
$bridge = $null
$server = $null
$writer = $null
$prepared = $false
$oldWslEnv = $env:WSLENV
Add-Type @'
using System;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
public sealed class PhuxBridgeFlood {
    public long CompletedBytes;
    public string Failure;
    public Task Completion;
    public PhuxBridgeFlood(Stream stream) {
        Completion = Task.Run(() => {
            byte[] bytes = new byte[65536];
            // An invalid opening length is deliberately forwarded unchanged;
            // this socket is a non-reading fixture, not a protocol server.
            for (int i = 0; i < bytes.Length; i++) bytes[i] = 255;
            try {
                for (int i = 0; i < 4096; i++) {
                    stream.Write(bytes, 0, bytes.Length);
                    Interlocked.Add(ref CompletedBytes, bytes.Length);
                }
            } catch (Exception error) { Failure = error.ToString(); }
        });
    }
    public long BytesWritten { get { return Interlocked.Read(ref CompletedBytes); } }
}
'@

function Start-BridgeWsl([string] $Script) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Script))
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = (Get-Command wsl.exe -CommandType Application).Source
    # Only the fixed shell trampoline and base64 cross Windows argv parsing.
    $start.Arguments = '--distribution ' + $env:PHUX_WSL_DISTRO + ' --exec sh -c "IFS=;set -f;eval $(printf %s ' + $encoded + '|base64 -d)"'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $start
    if (-not $process.Start()) { throw 'Could not start owned WSL fixture process' }
    return $process
}

function Get-BridgeStatus {
    return Invoke-DesktopWsl ('cat ' + (ConvertTo-DesktopShellLiteral "$root/status.json")) | ConvertFrom-Json
}

function Wait-Bridge([string] $Label, [scriptblock] $Condition, [int] $Seconds = 20) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    $last = ''
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($server.HasExited) { throw "Owned unread-socket fixture exited: $($server.ExitCode)" }
        try { if (& $Condition) { return } } catch { $last = $_.Exception.Message }
        Start-Sleep -Milliseconds 200
    }
    throw "Timed out waiting for ${Label}: $last"
}

function Assert-Backpressure {
    Wait-Bridge 'Linux bridge peer PID and unread socket queue' {
        $status = Get-BridgeStatus
        $status.accepted -and $status.bridgeAlive -and $status.queuedBytes -ge 4096
    }
    Wait-Bridge 'sustained native stdin backpressure' {
        $bytes = $writer.BytesWritten
        Start-Sleep -Seconds 2
        -not $bridge.HasExited -and -not $writer.Completion.IsCompleted -and $writer.BytesWritten -eq $bytes
    }
    $status = Get-BridgeStatus
    if (-not $status.bridgeAlive -or $status.queuedBytes -lt 4096) { throw 'Linux bridge must be alive with an unread socket before killing its Windows launcher' }
    return $status
}

try {
    # Do not import a caller's Windows HOME, socket or production profile.
    $env:WSLENV = ''
    $identity = Invoke-DesktopWsl 'printf "%s:%s:%s" "$(id -un)" "$(id -u)" "$HOME"'
    if ($identity -cnotmatch '^phux-ci:[1-9][0-9]*:/home/phux-ci$') { throw 'Bridge qualification requires the disposable phux-ci default WSL user' }
    $binary = ConvertTo-DesktopShellLiteral ([IO.Path]::GetFullPath($ServerBinary))
    $fixture = ConvertTo-DesktopShellLiteral (Join-Path $PSScriptRoot 'test-windows-bridge-fixture.py')
    Invoke-DesktopWsl ('set -eu; mkdir -m 700 ' + (ConvertTo-DesktopShellLiteral $root) + '; cp "$(wslpath -a -u ' + $binary + ')" ' + (ConvertTo-DesktopShellLiteral "$root/phux") + '; chmod 755 ' + (ConvertTo-DesktopShellLiteral "$root/phux") + '; cp "$(wslpath -a -u ' + $fixture + ')" ' + (ConvertTo-DesktopShellLiteral "$root/fixture.py")) | Out-Null
    $prepared = $true
    $server = Start-BridgeWsl ('exec python3 ' + (ConvertTo-DesktopShellLiteral "$root/fixture.py") + ' serve ' + (ConvertTo-DesktopShellLiteral $root))
    $serverErrors = $server.StandardError.ReadToEndAsync()
    Wait-Bridge 'owned Unix socket listener' { (Get-BridgeStatus).serverPid -gt 0 }
    $command = 'unset PHUX_SOCKET PHUX_BIN PHUX_SESSION PHUX_WS_TOKENS PHUX_WS_TLS_CERT PHUX_WS_TLS_KEY; export PHUX_PROFILE=windows-bridge-ci; exec ' + (ConvertTo-DesktopShellLiteral "$root/phux") + ' --socket ' + (ConvertTo-DesktopShellLiteral "$root/server.sock") + ' stdio-bridge'
    $bridge = Start-BridgeWsl $command
    $bridgeErrors = $bridge.StandardError.ReadToEndAsync()
    $writer = [PhuxBridgeFlood]::new($bridge.StandardInput.BaseStream)
    $before = Assert-Backpressure
    $before | ConvertTo-Json | Set-Content (Join-Path $Output 'bridge-before.json') -Encoding UTF8
    $windowsPid = $bridge.Id
    $clock = [Diagnostics.Stopwatch]::StartNew()
    # The only process killed in the regression itself is this recorded native
    # child. Do not mistake wsl.exe exit for proof its Linux bridge also exited.
    Stop-Process -Id $windowsPid -Force
    if (-not $bridge.WaitForExit(5000)) { throw 'Recorded native WSL launcher did not exit' }
    Wait-Bridge 'recorded Linux bridge exits after native launcher death' {
        $status = Get-BridgeStatus
        $status.bridgePid -eq $before.bridgePid -and $status.bridgeStart -ceq $before.bridgeStart -and -not $status.bridgeAlive
    } 65 # Includes the 5 seconds above: writes and final flush each have 30s bounds.
    $clock.Stop()
    if ($clock.Elapsed.TotalSeconds -gt 70) { throw 'Linux bridge exceeded the 70-second teardown bound' }
    @{ ok = $true; windowsPid = $windowsPid; linuxPid = $before.bridgePid; queuedBytes = $before.queuedBytes; completedNativeBytes = $writer.BytesWritten; elapsedSeconds = $clock.Elapsed.TotalSeconds; boundSeconds = 70; scenario = 'native wsl.exe death while stdin-to-Unix-socket is backpressured' } | ConvertTo-Json | Set-Content (Join-Path $Output 'bridge-teardown.result.json') -Encoding UTF8
} catch {
    $_ | Out-String | Set-Content (Join-Path $Output 'bridge-teardown.failure.txt') -Encoding UTF8
    throw
} finally {
    try {
        if ($bridge -and -not $bridge.HasExited) { Stop-Process -Id $bridge.Id -Force; $bridge.WaitForExit() }
        if ($prepared) { Invoke-DesktopWsl ('python3 ' + (ConvertTo-DesktopShellLiteral "$root/fixture.py") + ' cleanup ' + (ConvertTo-DesktopShellLiteral $root)) | Out-Null }
        if ($server -and -not $server.WaitForExit(5000)) { Stop-Process -Id $server.Id -Force; $server.WaitForExit() }
        if ($writer) { $writer.Completion.Wait(5000) | Out-Null }
        if ($bridgeErrors) { $bridgeErrors.Result | Set-Content (Join-Path $Output 'bridge.stderr.log') -Encoding UTF8 }
        if ($serverErrors) { $serverErrors.Result | Set-Content (Join-Path $Output 'bridge-fixture.stderr.log') -Encoding UTF8 }
        if ($prepared) { Invoke-DesktopWsl ('rm -rf ' + (ConvertTo-DesktopShellLiteral $root)) | Out-Null }
    } finally {
        $env:WSLENV = $oldWslEnv
    }
}
