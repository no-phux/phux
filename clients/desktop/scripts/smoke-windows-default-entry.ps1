#Requires -Version 5.1
# Default-argument qualification shares only the disposable smoke harness's
# GUI primitives, same-source coordinator and isolated Windows state.
if ($MyInvocation.InvocationName -ne '.') { throw 'Dot-source this helper from smoke-windows.ps1; it is not a standalone qualification entry' }

function Send-ControlShift([System.UInt16] $Key) {
    [PhuxDesktopGui]::Key(17, $false); [PhuxDesktopGui]::Key(16, $false)
    try { [PhuxDesktopGui]::Press($Key) }
    finally { [PhuxDesktopGui]::Key(16, $true); [PhuxDesktopGui]::Key(17, $true) }
}

function Assert-DefaultLaunch([string] $Label) {
    $pattern = 'launch socket=' + [regex]::Escape($socket) + ' session=(?<session>.+) phux=phux error=None'
    Wait-Desktop 'automatic CLI, socket and home resolution' {
        $log = Get-Content -Raw (Join-Path $Output "$Label.stderr.log")
        $log -match $pattern
    }
    return [regex]::Match((Get-Content -Raw (Join-Path $Output "$Label.stderr.log")), $pattern).Groups['session'].Value
}

function Get-SavedTab([string] $Kind) {
    $state = Join-Path $env:LOCALAPPDATA 'phux-desktop/wsl'
    foreach ($file in @(Get-ChildItem -LiteralPath $state -Filter layout.json -Recurse -ErrorAction SilentlyContinue)) {
        $layout = Get-Content -LiteralPath $file.FullName -Raw | ConvertFrom-Json
        $tab = @($layout.tabs | Where-Object { $_.root.kind -ceq $Kind -and $_.id -ceq $layout.activeTab })
        if ($tab.Count -eq 1) { return @{ Path = $file.FullName; Layout = $layout; Tab = $tab[0] } }
    }
    return $null
}

function Assert-DefaultDiscovery {
    if ($env:PHUX_BIN -or $env:PHUX_SOCKET -or $env:PHUX_SESSION) { throw 'Default-entry qualification must not pin a CLI, socket or session in its environment' }
    if ((Invoke-DesktopWsl 'printf "%s" "$HOME"') -cne '/home/phux-ci') { throw 'Windows test state must not replace the coordinator user home' }
    if ((Invoke-DesktopWsl 'export PATH="$HOME/.local/bin:$PATH"; command -v phux') -cne $phux) { throw 'Automatic CLI discovery must select the same-source disposable user installation' }
}

function Test-DefaultEntry {
    Stop-SmokeDesktop
    Assert-DefaultDiscovery
    # The explicit-target phase has its own saved workspace. Start this phase
    # with no prior layouts so a stale file cannot satisfy the split assertions.
    $state = Join-Path $env:LOCALAPPDATA 'phux-desktop/wsl'
    if (Test-Path -LiteralPath $state) { Remove-Item -LiteralPath $state -Recurse -Force }
    # Exactly the installed shortcut's target and empty argument list, not the
    # qualification-only --phux/--socket/--session/--fresh/--evidence entry.
    Start-SmokeDesktop 'default-entry' -DefaultEntry
    $homeSession = Assert-DefaultLaunch 'default-entry'
    Wait-Desktop 'default home layout saved' { $null -ne (Get-SavedTab 'leaf') }
    $initialTab = (Get-SavedTab 'leaf').Tab.id
    Send-ControlShift 84 # New terminal in the automatically selected home.
    Wait-Desktop 'new terminal attached and saved' {
        $created = Get-SavedTab 'leaf'
        $null -ne $created -and $created.Tab.id -cne $initialTab
    }
    Send-Shell ('PHUX_DEFAULT_VALUE=PHUX"LEFT"; printf "\033[2J\033[H    %s\n    %s\n    %s\n" "$PHUX_DEFAULT_VALUE" "$PHUX_DEFAULT_VALUE" "$PHUX_DEFAULT_VALUE"; printf "%s\n" "$$" > ' + "$linuxRoot/default-left.pid")
    Assert-Rendered 'default-left' 'PHUXLEFT'
    Send-ControlShift 68 # Windows Split Right, a local presentation to restore.
    Wait-Desktop 'native split attached and saved' { $null -ne (Get-SavedTab 'split') }
    Send-Shell ('PHUX_DEFAULT_VALUE=PHUX"RIGHT"; printf "\033[2J\033[H    %s\n    %s\n    %s\n" "$PHUX_DEFAULT_VALUE" "$PHUX_DEFAULT_VALUE" "$PHUX_DEFAULT_VALUE"; printf "%s\n" "$$" > ' + "$linuxRoot/default-right.pid")
    Assert-Rendered 'default-right' 'PHUXRIGHT'
    $saved = Get-SavedTab 'split'
    $saved.Layout | ConvertTo-Json -Depth 30 | Set-Content (Join-Path $Output 'default-entry.saved-layout.json') -Encoding UTF8
    $expectedTabs = $saved.Layout.tabs | ConvertTo-Json -Depth 30 -Compress
    $right = $saved.Tab.focusedTerminal
    $rightPid = Invoke-DesktopWsl ('cat ' + (ConvertTo-DesktopShellLiteral "$linuxRoot/default-right.pid"))
    Stop-SmokeDesktop # Forced native death: no graceful on-close save can rescue it.
    Invoke-SmokeCli @('send-keys', $right, ('printf "\033[2J\033[H    %s\n    %s\n    %s\n" "$PHUX_DEFAULT_VALUE" "$PHUX_DEFAULT_VALUE" "$PHUX_DEFAULT_VALUE"; printf "%s\n" "$$" > ' + "$linuxRoot/default-after.pid"), 'Enter') | Out-Null
    Wait-Desktop 'default-entry shell survives app crash' { (Invoke-DesktopWsl ('test -s ' + (ConvertTo-DesktopShellLiteral "$linuxRoot/default-after.pid") + ' && cat ' + (ConvertTo-DesktopShellLiteral "$linuxRoot/default-after.pid"))) -ceq $rightPid }
    Start-SmokeDesktop 'default-relaunch' -DefaultEntry
    if ((Assert-DefaultLaunch 'default-relaunch') -cne $homeSession) { throw 'Default relaunch selected a different home session' }
    # Both panes must be visible, not merely alive on the coordinator or rebuilt
    # as unrelated tabs. The saved active tab and split identities must survive.
    Assert-Rendered 'default-restored-left' 'PHUXLEFT'
    Assert-Rendered 'default-restored-right' 'PHUXRIGHT'
    $restored = Get-SavedTab 'split'
    if ($null -eq $restored -or ($restored.Layout.tabs | ConvertTo-Json -Depth 30 -Compress) -cne $expectedTabs) { throw 'Default relaunch did not preserve the saved Windows tabs and split identities' }
    if ((Invoke-SmokeCli @('status', '--json') | ConvertFrom-Json).pid -ne $serverPid) { throw 'Default entry changed the same-source coordinator' }
    @{ arguments = @(); phuxBin = $env:PHUX_BIN; phuxSocket = $env:PHUX_SOCKET; profile = $profile; socket = $socket; session = $homeSession; linuxHome = '/home/phux-ci'; cli = $phux; savedLayout = $saved.Path; shellPid = $rightPid; restoredTabs = $restored.Layout.tabs } | ConvertTo-Json -Depth 30 | Set-Content (Join-Path $Output 'default-entry.result.json') -Encoding UTF8
}
