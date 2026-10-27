#Requires -Version 5.1
# Real installed native GUI against an isolated SAME-SOURCE coordinator in WSL.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string] $Archive,
    [Parameter(Mandatory = $true)][string] $ServerBinary,
    [Parameter(Mandatory = $true)][string] $Output
)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Run GUI qualification on a disposable Windows CI runner, not your installed desktop' }
if (Test-Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\PhuxDesktop') { throw 'Qualification refuses an existing desktop installation' }
if (-not [Environment]::UserInteractive) { throw 'Native GUI qualification requires an interactive desktop; it cannot be skipped' }
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
. (Join-Path $repo 'scripts/install-desktop-windows.ps1')
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Threading;
public static class PhuxDesktopGui {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort vk, scan; public uint flags, time; public UIntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int x, y; public uint data, flags, time; public UIntPtr extra; }
    [StructLayout(LayoutKind.Explicit)] struct UNION { [FieldOffset(0)] public KEYBDINPUT key; [FieldOffset(0)] public MOUSEINPUT mouse; }
    [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public UNION value; }
    [DllImport("user32.dll", SetLastError=true)] static extern uint SendInput(uint count, INPUT[] inputs, int size);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window, int mode);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out RECT rect);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    public static void Key(ushort vk, bool up) {
        INPUT input = new INPUT { type = 1, value = new UNION { key = new KEYBDINPUT { vk = vk, flags = up ? 2u : 0u } } };
        Send(input);
    }
    static void Send(INPUT input) {
        if (SendInput(1, new INPUT[] { input }, Marshal.SizeOf(typeof(INPUT))) != 1) throw new System.ComponentModel.Win32Exception();
    }
    public static void Press(ushort vk) { Key(vk, false); Key(vk, true); }
    public static void Text(string text) {
        foreach (char c in text) {
            Send(new INPUT { type = 1, value = new UNION { key = new KEYBDINPUT { scan = c, flags = 4 } } });
            Send(new INPUT { type = 1, value = new UNION { key = new KEYBDINPUT { scan = c, flags = 6 } } });
            Thread.Sleep(12);
        }
    }
    public static void Click(int x, int y) {
        SetCursorPos(x, y);
        Send(new INPUT { type = 0, value = new UNION { mouse = new MOUSEINPUT { flags = 2 } } });
        Send(new INPUT { type = 0, value = new UNION { mouse = new MOUSEINPUT { flags = 4 } } });
    }
}
'@
$Output = [IO.Path]::GetFullPath($Output)
New-Item -ItemType Directory -Force $Output | Out-Null
$work = Join-Path ([IO.Path]::GetTempPath()) ('phux-native-smoke-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $work | Out-Null
$linuxRoot = '/tmp/phux-desktop-ci-' + [Guid]::NewGuid().ToString('N')
$profile = 'windows-desktop-smoke-' + [Guid]::NewGuid().ToString('N')
$socket = $null
$phux = '/home/phux-ci/.local/bin/phux'
$session = 'windows-desktop-smoke'
$installed = Join-Path $work 'installed'
$desktop = $null
$started = $false
$uninstall = $false
$copiedCli = $false
$savedEnvironment = @{}
foreach ($name in @('USERPROFILE', 'HOME', 'LOCALAPPDATA', 'APPDATA', 'WSLENV', 'PHUX_BIN', 'PHUX_SESSION', 'PHUX_SOCKET', 'PHUX_PROFILE', 'PHUX_WS_TOKENS', 'PHUX_WS_TLS_CERT', 'PHUX_WS_TLS_KEY', 'PHUX_DESKTOP_BACKGROUND', 'PHUX_DESKTOP_LOG', 'PHUX_DESKTOP_LOG_LEVEL')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
$tesseract = Join-Path $env:ProgramFiles 'Tesseract-OCR/tesseract.exe'
if (-not (Test-Path $tesseract)) { throw 'Tesseract OCR is required to prove native terminal rendering' }

function Wait-Desktop([string] $Description, [scriptblock] $Condition) {
    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    $last = ''
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($script:desktop -and $script:desktop.HasExited) { throw "Desktop exited while waiting for $Description (exit $($script:desktop.ExitCode))" }
        try { if (& $Condition) { return } } catch { $last = $_.Exception.Message }
        Start-Sleep -Milliseconds 200
    }
    throw "Timed out waiting for ${Description}: $last"
}

function Invoke-SmokeCli([string[]] $Arguments) {
    $words = @($phux) + $Arguments
    $quoted = @($words | ForEach-Object { ConvertTo-DesktopShellLiteral $_ }) -join ' '
    $environment = 'unset PHUX_SOCKET PHUX_BIN PHUX_SESSION PHUX_WS_TOKENS PHUX_WS_TLS_CERT PHUX_WS_TLS_KEY; export PHUX_PROFILE=' + (ConvertTo-DesktopShellLiteral $profile) + ' SHELL=/bin/sh; '
    return Invoke-DesktopWsl ($environment + $quoted)
}

function Start-SmokeDesktop([string] $Label, [switch] $DefaultEntry) {
    $evidence = Join-Path $Output $Label
    New-Item -ItemType Directory -Force $evidence | Out-Null
    $executable = Join-Path $installed 'current/phux-desktop.exe'
    $arguments = @('--socket', $socket, '--phux', $phux, '--session', $session, '--fresh', '--evidence', ('"' + $evidence + '"'))
    $launch = @{ FilePath = $executable; PassThru = $true; RedirectStandardOutput = (Join-Path $Output "$Label.stdout.log"); RedirectStandardError = (Join-Path $Output "$Label.stderr.log") }
    if (-not $DefaultEntry) { $launch.ArgumentList = $arguments }
    $script:desktop = Start-Process @launch
    Wait-Desktop 'native HWND' { $script:desktop.Refresh(); $script:desktop.MainWindowHandle -ne [IntPtr]::Zero }
    $window = $script:desktop.MainWindowHandle
    [PhuxDesktopGui]::ShowWindow($window, 9) | Out-Null
    [PhuxDesktopGui]::SetWindowPos($window, [IntPtr]::Zero, 10, 10, 760, 520, 0x0040) | Out-Null
    if (-not [PhuxDesktopGui]::SetForegroundWindow($window)) { throw 'Could not activate the real native window' }
    Start-Sleep -Milliseconds 500
    if ($DefaultEntry) { return }
    [PhuxDesktopGui]::Click(500, 320)
    Assert-FrameReceipt $evidence
}

function Assert-FrameReceipt([string] $Evidence) {
    Wait-Desktop 'native submission receipt' { Test-Path (Join-Path $Evidence 'interaction.json') }
    $layout = Get-Content -Raw (Join-Path $Evidence 'layout.json') | ConvertFrom-Json
    $interaction = Get-Content -Raw (Join-Path $Evidence 'interaction.json') | ConvertFrom-Json
    if ($layout.session -cne $session -or $layout.generation -le 0 -or -not ($layout.presented -or $layout.submitted)) { throw 'Native frame has no truthful presentation/submission receipt' }
    if (-not $interaction.visible -or $interaction.clipped -or $interaction.presentedGeneration -ne $layout.generation) { throw 'Native capture and visible layout generations disagree' }
}

function Get-WindowText([string] $Label) {
    $rect = New-Object PhuxDesktopGui+RECT
    if (-not [PhuxDesktopGui]::GetWindowRect($script:desktop.MainWindowHandle, [ref] $rect)) { throw 'Could not read the native window bounds' }
    $screen = [Windows.Forms.Screen]::PrimaryScreen.Bounds
    $left = [Math]::Max(0, $rect.Left); $top = [Math]::Max(0, $rect.Top)
    $width = [Math]::Min($rect.Right, $screen.Right) - $left
    $height = [Math]::Min($rect.Bottom, $screen.Bottom) - $top
    if ($width -le 0 -or $height -le 0) { throw 'Native window is not on the display' }
    $bitmap = [Drawing.Bitmap]::new($width, $height)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    $png = Join-Path $Output "$Label.window.png"
    try { $graphics.CopyFromScreen($left, $top, 0, 0, $bitmap.Size); $bitmap.Save($png, [Drawing.Imaging.ImageFormat]::Png) }
    finally { $graphics.Dispose(); $bitmap.Dispose() }
    $text = (& $tesseract $png stdout --psm 11 -l eng 2>$null) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'Window OCR failed' }
    [IO.File]::WriteAllText((Join-Path $Output "$Label.ocr.txt"), $text)
    return $text
}

function Assert-Rendered([string] $Label, [string] $Marker) {
    Wait-Desktop "$Label rendered on Windows" { (Get-WindowText $Label).Contains($Marker) }
}

function Send-Shell([string] $Text) {
    [PhuxDesktopGui]::Text($Text)
    [PhuxDesktopGui]::Press(13)
}

function Stop-SmokeDesktop {
    if ($script:desktop -and -not $script:desktop.HasExited) {
        # Exactly the child this harness started, never a process name or port.
        Stop-Process -Id $script:desktop.Id -Force
        $script:desktop.WaitForExit()
    }
    $script:desktop = $null
}

function Get-ConnectionEpoch {
    $log = Get-Content -Raw (Join-Path $Output 'first-launch.stderr.log')
    $epochs = [regex]::Matches($log, 'connection [^\r\n]* -> Attached epoch=(\d+)')
    if ($epochs.Count -eq 0) { return -1 }
    return [long] $epochs[$epochs.Count - 1].Groups[1].Value
}

. (Join-Path $PSScriptRoot 'smoke-windows-default-entry.ps1')

try {
    # WSL does not normally inherit arbitrary Windows variables. Forward only
    # this run's unique profile, never Windows HOME or inherited production sockets.
    foreach ($name in @('PHUX_BIN', 'PHUX_SESSION', 'PHUX_SOCKET', 'PHUX_WS_TOKENS', 'PHUX_WS_TLS_CERT', 'PHUX_WS_TLS_KEY', 'PHUX_DESKTOP_BACKGROUND', 'PHUX_DESKTOP_LOG')) { [Environment]::SetEnvironmentVariable($name, $null, 'Process') }
    $env:PHUX_PROFILE = $profile
    $env:WSLENV = 'PHUX_PROFILE/u'
    $identity = Invoke-DesktopWsl 'printf "%s:%s:%s" "$(id -un)" "$(id -u)" "$HOME"'
    if ($identity -cnotmatch '^phux-ci:[1-9][0-9]*:/home/phux-ci$') { throw 'Qualification requires the disposable phux-ci default WSL user and its real home' }
    if ((Invoke-DesktopWsl 'printf "%s" "$PHUX_PROFILE"') -cne $profile) { throw 'WSL did not inherit the isolated qualification profile' }
    $hostBinary = ConvertTo-DesktopShellLiteral ([IO.Path]::GetFullPath($ServerBinary))
    Invoke-DesktopWsl ('set -eu; test ! -e ' + (ConvertTo-DesktopShellLiteral $phux) + '; test ! -L ' + (ConvertTo-DesktopShellLiteral $phux) + '; mkdir -m 700 ' + (ConvertTo-DesktopShellLiteral $linuxRoot) + '; mkdir -p "$HOME/.local/bin"; cp -n "$(wslpath -a -u ' + $hostBinary + ')" ' + (ConvertTo-DesktopShellLiteral $phux) + '; chmod 755 ' + (ConvertTo-DesktopShellLiteral $phux)) | Out-Null
    $copiedCli = $true
    Invoke-SmokeCli @('server', '--ensure') | Out-Null
    $started = $true
    $socket = (Invoke-SmokeCli @('status', '--json') | ConvertFrom-Json).socket
    if (-not $socket.StartsWith('/') -or -not $socket.Contains($profile)) { throw 'CLI default socket is not scoped to the unique qualification profile' }
    Invoke-SmokeCli @('new', '--json', '--session', $session) | Out-Null
    $serverPid = (Invoke-SmokeCli @('status', '--json') | ConvertFrom-Json).pid
    $initialize = 'PHUX_SMOKE_VALUE=PHUXSURVIVAL; printf "\033[2J\033[H%s\n" "$PHUX_SMOKE_VALUE"; printf "%s\n" "$$" > ' + "$linuxRoot/shell.pid"
    Invoke-SmokeCli @('send-keys', $session, $initialize, 'Enter') | Out-Null
    Wait-Desktop 'initial WSL shell marker' { (Invoke-SmokeCli @('snapshot', $session)).Contains('PHUXSURVIVAL') }
    $shellPid = Invoke-DesktopWsl ('cat ' + (ConvertTo-DesktopShellLiteral "$linuxRoot/shell.pid"))
    $fields = ([IO.File]::ReadAllText($Archive + '.sha256').Trim() -split '\s+')
    if ($fields.Count -ne 2 -or $fields[1] -cne [IO.Path]::GetFileName($Archive) -or (Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLowerInvariant() -cne $fields[0]) { throw 'Build artifact checksum does not match its sidecar' }
    $bundle = Expand-DesktopArchive $Archive (Join-Path $work 'unpacked')
    $release = Get-DesktopBundle $bundle
    $expectedSha = (& git -C $repo rev-parse HEAD) -join ''
    $expectedVersion = (Get-Content -Raw (Join-Path $repo 'clients/desktop/package.json') | ConvertFrom-Json).version
    if ($release.sha -cne $expectedSha -or $release.version -cne $expectedVersion) { throw 'Packaged native app does not identify this same-source checkout' }
    Install-DesktopBundle $bundle $installed (Join-Path $repo 'scripts/install-desktop-windows.ps1') $true
    $uninstall = $true
    $registered = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\PhuxDesktop'
    if ($registered.InstallLocation -cne $installed -or $registered.DisplayVersion -cne $release.version) { throw 'Per-user uninstall registration is incorrect' }
    $shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Phux Desktop.lnk'
    $link = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut)
    if ($link.TargetPath -ine (Join-Path $installed 'current/phux-desktop.exe')) { throw 'Start Menu shortcut does not launch the installed native executable' }
    if ($link.Arguments) { throw 'Installed shortcut must use the normal empty argument list' }
    $env:USERPROFILE = Join-Path $work 'home'
    $env:HOME = $env:USERPROFILE
    $env:LOCALAPPDATA = Join-Path $work 'local'
    $env:APPDATA = Join-Path $work 'roaming'
    foreach ($directory in @($env:USERPROFILE, $env:LOCALAPPDATA, $env:APPDATA)) { New-Item -ItemType Directory -Force $directory | Out-Null }
    $env:PHUX_DESKTOP_LOG_LEVEL = 'trace'
    Start-SmokeDesktop 'first-launch'
    Assert-Rendered 'first-launch' 'PHUXSURVIVAL'
    Send-Shell "printf '%s%s\n' PHUX NATIVEKEYS"
    Assert-Rendered 'native-keys' 'PHUXNATIVEKEYS'
    if (-not (Invoke-SmokeCli @('snapshot', $session)).Contains('PHUXNATIVEKEYS')) { throw 'Native keys did not reach the WSL shell' }
    Set-Clipboard -Value "printf '%s%s\n' PHUX CLIPBOARD"
    [PhuxDesktopGui]::Key(17, $false); [PhuxDesktopGui]::Key(16, $false)
    [PhuxDesktopGui]::Press(86)
    [PhuxDesktopGui]::Key(16, $true); [PhuxDesktopGui]::Key(17, $true)
    [PhuxDesktopGui]::Press(13)
    Assert-Rendered 'clipboard' 'PHUXCLIPBOARD'
    Send-Shell 'printf ''BEFORE_%s\n'' "$(stty size)"'
    Wait-Desktop 'initial PTY size' { (Invoke-SmokeCli @('snapshot', $session)) -match 'BEFORE_(\d+) (\d+)' }
    $before = [regex]::Match((Invoke-SmokeCli @('snapshot', $session)), 'BEFORE_(\d+) (\d+)')
    $screen = [Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    [PhuxDesktopGui]::SetWindowPos($desktop.MainWindowHandle, [IntPtr]::Zero, 0, 0, $screen.Width - 10, $screen.Height - 10, 0x0040) | Out-Null
    Start-Sleep -Seconds 1
    Send-Shell 'printf ''AFTER_%s\n'' "$(stty size)"'
    Wait-Desktop 'resized PTY' {
        $after = [regex]::Match((Invoke-SmokeCli @('snapshot', $session)), 'AFTER_(\d+) (\d+)')
        $after.Success -and [int] $after.Groups[1].Value -gt [int] $before.Groups[1].Value -and [int] $after.Groups[2].Value -gt [int] $before.Groups[2].Value
    }
    # Windows' reconnect binding: exercise the real action, not a CLI-only round trip.
    $epoch = Get-ConnectionEpoch
    if ($epoch -lt 0) { throw 'Native connection epoch must be observable before reconnect qualification' }
    [PhuxDesktopGui]::Key(17, $false); [PhuxDesktopGui]::Key(16, $false); [PhuxDesktopGui]::Press(82)
    [PhuxDesktopGui]::Key(16, $true); [PhuxDesktopGui]::Key(17, $true)
    Wait-Desktop 'UI reconnect attached a new epoch' { (Get-ConnectionEpoch) -gt $epoch }
    Send-Shell "printf '%s%s\n' PHUX RECONNECTED"
    Assert-Rendered 'reconnected' 'PHUXRECONNECTED'
    Stop-SmokeDesktop
    $afterCrash = 'printf "\033[2J\033[H%s%s\n" "$PHUX_SMOKE_VALUE" AFTERCRASH; printf "%s\n" "$$" > ' + "$linuxRoot/after.pid"
    Invoke-SmokeCli @('send-keys', $session, $afterCrash, 'Enter') | Out-Null
    Wait-Desktop 'surviving shell after Windows app death' { (Invoke-SmokeCli @('snapshot', $session)).Contains('PHUXSURVIVALAFTERCRASH') }
    if ((Invoke-DesktopWsl ('cat ' + (ConvertTo-DesktopShellLiteral "$linuxRoot/after.pid"))) -cne $shellPid) { throw 'The original shell did not survive' }
    if ((Invoke-SmokeCli @('status', '--json') | ConvertFrom-Json).pid -ne $serverPid) { throw 'The original server did not survive' }
    Start-SmokeDesktop 'after-crash'
    Assert-Rendered 'after-crash' 'PHUXSURVIVALAFTERCRASH'
    Test-DefaultEntry
    $renderer = Get-Content -Raw (Join-Path $Output 'first-launch.stderr.log')
    $gpu = [regex]::Match($renderer, 'Using GPU: ([^\r\n]+)').Groups[1].Value
    if (-not $gpu -or $renderer -notmatch 'Created device with Direct3D') { throw 'Native D3D adapter/feature-level evidence is missing' }
    @{ ok = $true; platform = 'windows-x64'; gpu = $gpu; qualification = 'hosted native D3D GUI, not real-GPU qualification'; serverPid = $serverPid; shellPid = $shellPid; scenarios = @('visible window OCR', 'native keys', 'clipboard', 'PTY resize', 'UI reconnect', 'app crash/relaunch', 'shell persistence', 'installed shortcut default arguments', 'automatic WSL CLI/socket/home selection', 'saved Windows split restoration after crash') } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $Output 'result.json') -Encoding UTF8
} catch {
    $_ | Out-String | Set-Content (Join-Path $Output 'failure.txt') -Encoding UTF8
    throw
} finally {
    Stop-SmokeDesktop
    try {
        if ($started) { Invoke-SmokeCli @('kill', '--server') | Out-Null }
        if ($uninstall) { Uninstall-Desktop $installed }
        if ($copiedCli) { Invoke-DesktopWsl ('rm -f ' + (ConvertTo-DesktopShellLiteral $phux)) | Out-Null }
        Invoke-DesktopWsl ('rm -rf ' + (ConvertTo-DesktopShellLiteral $linuxRoot)) | Out-Null
    } finally {
        foreach ($item in $savedEnvironment.GetEnumerator()) { [Environment]::SetEnvironmentVariable($item.Key, $item.Value, 'Process') }
    }
}
