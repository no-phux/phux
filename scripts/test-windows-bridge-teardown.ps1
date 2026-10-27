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
    $start.FileName = (Get-Command wsl.exe -CommandType Application | Select-Object -First 1).Source
    # Only the fixed shell trampoline and base64 cross Windows argv parsing.
    $start.Arguments = '--distribution ' + $env:PHUX_WSL_DISTRO + ' --exec sh -c "IFS=;set -f;eval $(printf %s ' + $encoded + '|base64 -d)"'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $start
    try {
        if (-not $process.Start()) { throw 'Could not start owned WSL fixture process' }
        return $process
    } catch { $process.Dispose(); throw }
}

function Get-BridgeWaitMilliseconds([DateTime] $Deadline, [int] $Maximum = 5000) {
    return [int] [Math]::Max(0, [Math]::Min($Maximum, [Math]::Ceiling(($Deadline - [DateTime]::UtcNow).TotalMilliseconds)))
}

function Stop-BridgeChild([Diagnostics.Process] $Child, [DateTime] $Deadline) {
    if ($null -eq $Child) { return }
    if ($Child.HasExited) { return }
    # This Process was returned by Start-BridgeWsl; never select by name/port.
    $ownedPid = $Child.Id
    Stop-Process -Id $ownedPid -Force
    if (-not $Child.WaitForExit((Get-BridgeWaitMilliseconds $Deadline))) { throw "Owned WSL child $ownedPid did not exit before its deadline" }
}

function Receive-BridgeDrain([Threading.Tasks.Task[string]] $Drain, [DateTime] $Deadline) {
    if (-not $Drain.Wait((Get-BridgeWaitMilliseconds $Deadline))) { throw 'Owned WSL output drain timed out' }
    # GetResult is safe only after the bounded wait proved completion.
    return $Drain.GetAwaiter().GetResult()
}

function Invoke-BridgeWsl([string] $Script, [DateTime] $Deadline = [DateTime]::MaxValue, [int] $TimeoutSeconds = 5) {
    $probeDeadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    if ($Deadline -lt $probeDeadline) { $probeDeadline = $Deadline }
    if ((Get-BridgeWaitMilliseconds $probeDeadline) -eq 0) { throw 'WSL probe has no remaining deadline budget' }
    $child = Start-BridgeWsl $Script
    try {
        # Start both readers before waiting: neither full pipe can wedge exit.
        $stdout = $child.StandardOutput.ReadToEndAsync()
        $stderr = $child.StandardError.ReadToEndAsync()
        $child.StandardInput.Close()
        if (-not $child.WaitForExit((Get-BridgeWaitMilliseconds $probeDeadline ($TimeoutSeconds * 1000)))) { throw 'Owned WSL probe timed out' }
        $text = Receive-BridgeDrain $stdout $probeDeadline
        $detail = Receive-BridgeDrain $stderr $probeDeadline
        if ($child.ExitCode -ne 0) { throw "Owned WSL probe failed ($($child.ExitCode)): $detail" }
        return $text.TrimEnd([char[]] "`r`n")
    } finally {
        try { Stop-BridgeChild $child $probeDeadline }
        finally { $child.Dispose() }
    }
}

function Get-BridgeStatus([DateTime] $Deadline = [DateTime]::MaxValue) {
    return Invoke-BridgeWsl ('cat ' + (ConvertTo-DesktopShellLiteral "$root/status.json")) $Deadline | ConvertFrom-Json
}

function Wait-Bridge([string] $Label, [scriptblock] $Condition, [int] $Seconds = 20, [DateTime] $Deadline = [DateTime]::MinValue) {
    if ($Deadline -eq [DateTime]::MinValue) { $Deadline = [DateTime]::UtcNow.AddSeconds($Seconds) }
    $last = ''
    while ([DateTime]::UtcNow -lt $Deadline) {
        if ($server.HasExited) { throw "Owned unread-socket fixture exited: $($server.ExitCode)" }
        try { if ((& $Condition $Deadline) -and [DateTime]::UtcNow -lt $Deadline) { return } } catch { $last = $_.Exception.Message }
        Start-Sleep -Milliseconds (Get-BridgeWaitMilliseconds $Deadline 200)
    }
    throw "Timed out waiting for ${Label}: $last"
}

function Assert-Backpressure {
    Wait-Bridge 'Linux bridge peer PID and unread socket queue' {
        param($ProbeDeadline)
        $status = Get-BridgeStatus $ProbeDeadline
        $status.accepted -and $status.bridgeAlive -and $status.queuedBytes -ge 4096
    }
    Wait-Bridge 'sustained native stdin backpressure' {
        param($ProbeDeadline)
        $bytes = $writer.BytesWritten
        $pauseMilliseconds = Get-BridgeWaitMilliseconds $ProbeDeadline 2000
        if ($pauseMilliseconds -lt 2000) { return $false }
        Start-Sleep -Milliseconds $pauseMilliseconds
        -not $bridge.HasExited -and -not $writer.Completion.IsCompleted -and $writer.BytesWritten -eq $bytes
    }
    $status = Get-BridgeStatus
    if (-not $status.bridgeAlive -or $status.queuedBytes -lt 4096) { throw 'Linux bridge must be alive with an unread socket before killing its Windows launcher' }
    return $status
}

function Invoke-BridgeCleanupStep([scriptblock] $Action) {
    try { & $Action | Out-Null }
    catch { $cleanupErrors.Add($_.Exception.Message) }
}

function Save-BridgeDrain([Threading.Tasks.Task[string]] $Drain, [string] $Path) {
    if ($null -eq $Drain) { return }
    Receive-BridgeDrain $Drain ([DateTime]::UtcNow.AddSeconds(5)) | Set-Content $Path -Encoding UTF8
}

try {
    # Do not import a caller's Windows HOME, socket or production profile.
    $env:WSLENV = ''
    $identity = Invoke-BridgeWsl 'printf "%s:%s:%s" "$(id -un)" "$(id -u)" "$HOME"'
    if ($identity -cnotmatch '^phux-ci:[1-9][0-9]*:/home/phux-ci$') { throw 'Bridge qualification requires the disposable phux-ci default WSL user' }
    $binary = ConvertTo-DesktopShellLiteral ([IO.Path]::GetFullPath($ServerBinary))
    $fixture = ConvertTo-DesktopShellLiteral (Join-Path $PSScriptRoot 'test-windows-bridge-fixture.py')
    Invoke-BridgeWsl ('set -eu; mkdir -m 700 ' + (ConvertTo-DesktopShellLiteral $root) + '; cp "$(wslpath -a -u ' + $binary + ')" ' + (ConvertTo-DesktopShellLiteral "$root/phux") + '; chmod 755 ' + (ConvertTo-DesktopShellLiteral "$root/phux") + '; cp "$(wslpath -a -u ' + $fixture + ')" ' + (ConvertTo-DesktopShellLiteral "$root/fixture.py")) -TimeoutSeconds 30 | Out-Null
    $prepared = $true
    $server = Start-BridgeWsl ('exec python3 ' + (ConvertTo-DesktopShellLiteral "$root/fixture.py") + ' serve ' + (ConvertTo-DesktopShellLiteral $root))
    $serverErrors = $server.StandardError.ReadToEndAsync()
    Wait-Bridge 'owned Unix socket listener' { param($ProbeDeadline) (Get-BridgeStatus $ProbeDeadline).serverPid -gt 0 }
    $command = 'unset PHUX_SOCKET PHUX_BIN PHUX_SESSION PHUX_WS_TOKENS PHUX_WS_TLS_CERT PHUX_WS_TLS_KEY; export PHUX_PROFILE=windows-bridge-ci; exec ' + (ConvertTo-DesktopShellLiteral "$root/phux") + ' --socket ' + (ConvertTo-DesktopShellLiteral "$root/server.sock") + ' stdio-bridge'
    $bridge = Start-BridgeWsl $command
    $bridgeErrors = $bridge.StandardError.ReadToEndAsync()
    $writer = [PhuxBridgeFlood]::new($bridge.StandardInput.BaseStream)
    $before = Assert-Backpressure
    $before | ConvertTo-Json | Set-Content (Join-Path $Output 'bridge-before.json') -Encoding UTF8
    $windowsPid = $bridge.Id
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $teardownDeadline = [DateTime]::UtcNow.AddSeconds(70)
    # The only process killed in the regression itself is this recorded native
    # child. Do not mistake wsl.exe exit for proof its Linux bridge also exited.
    Stop-Process -Id $windowsPid -Force
    if (-not $bridge.WaitForExit((Get-BridgeWaitMilliseconds $teardownDeadline))) { throw 'Recorded native WSL launcher did not exit' }
    Wait-Bridge 'recorded Linux bridge exits after native launcher death' {
        param($ProbeDeadline)
        $status = Get-BridgeStatus $ProbeDeadline
        $status.bridgePid -eq $before.bridgePid -and $status.bridgeStart -ceq $before.bridgeStart -and -not $status.bridgeAlive
    } -Deadline $teardownDeadline # Launcher exit and all probes share the same 70s budget.
    $clock.Stop()
    if ($clock.Elapsed.TotalSeconds -gt 70) { throw 'Linux bridge exceeded the 70-second teardown bound' }
    @{ ok = $true; windowsPid = $windowsPid; linuxPid = $before.bridgePid; queuedBytes = $before.queuedBytes; completedNativeBytes = $writer.BytesWritten; elapsedSeconds = $clock.Elapsed.TotalSeconds; boundSeconds = 70; scenario = 'native wsl.exe death while stdin-to-Unix-socket is backpressured' } | ConvertTo-Json | Set-Content (Join-Path $Output 'bridge-teardown.result.json') -Encoding UTF8
} catch {
    $_ | Out-String | Set-Content (Join-Path $Output 'bridge-teardown.failure.txt') -Encoding UTF8
    throw
} finally {
    $cleanupErrors = [Collections.Generic.List[string]]::new()
    try {
        Invoke-BridgeCleanupStep { Stop-BridgeChild $bridge ([DateTime]::UtcNow.AddSeconds(5)) }
        if ($prepared) { Invoke-BridgeCleanupStep { Invoke-BridgeWsl ('python3 ' + (ConvertTo-DesktopShellLiteral "$root/fixture.py") + ' cleanup ' + (ConvertTo-DesktopShellLiteral $root)) } }
        Invoke-BridgeCleanupStep { Stop-BridgeChild $server ([DateTime]::UtcNow.AddSeconds(5)) }
        Invoke-BridgeCleanupStep { if ($writer -and -not $writer.Completion.Wait(5000)) { throw 'Owned bridge flood writer did not stop within five seconds' } }
        Invoke-BridgeCleanupStep { Save-BridgeDrain $bridgeErrors (Join-Path $Output 'bridge.stderr.log') }
        Invoke-BridgeCleanupStep { Save-BridgeDrain $serverErrors (Join-Path $Output 'bridge-fixture.stderr.log') }
        if ($prepared) { Invoke-BridgeCleanupStep { Invoke-BridgeWsl ('rm -rf ' + (ConvertTo-DesktopShellLiteral $root)) } }
    } finally {
        Invoke-BridgeCleanupStep { if ($bridge) { $bridge.Dispose() } }
        Invoke-BridgeCleanupStep { if ($server) { $server.Dispose() } }
        $env:WSLENV = $oldWslEnv
    }
    if ($cleanupErrors.Count -gt 0) {
        $cleanupErrors | Set-Content (Join-Path $Output 'bridge-cleanup.failure.txt') -Encoding UTF8
        throw ('Owned bridge cleanup failed: ' + ($cleanupErrors -join '; '))
    }
}
