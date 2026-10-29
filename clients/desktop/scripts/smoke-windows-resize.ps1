#Requires -Version 5.1
if ($MyInvocation.InvocationName -ne '.') { throw 'Dot-source from the disposable Windows smoke harness' }

function Test-NativeResize {
    if ($shellPid -cnotmatch '^[1-9][0-9]*$') { throw 'PTY measurement requires the recorded shell PID' }
    # Observe the original shell's actual tty without typing during a geometry
    # transition or attaching a second client that might cast a resize vote.
    $measure = "stty -F /proc/$shellPid/fd/0 size"
    $sizes = @{ Before = ''; After = '' }
    Wait-Desktop 'initial PTY size' {
        $sizes.Before = Invoke-DesktopWsl $measure
        $sizes.Before -match '^\d+ \d+$'
    }
    $before = [regex]::Match($sizes.Before, '^(\d+) (\d+)$')
    $rows = [int] $before.Groups[1].Value
    $cols = [int] $before.Groups[2].Value
    $screen = [Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    if (-not [PhuxDesktopGui]::SetWindowPos($desktop.MainWindowHandle, [IntPtr]::Zero, 0, 0, $screen.Width - 10, $screen.Height - 10, 0x0040)) { throw 'Native window resize failed' }
    Wait-Desktop 'resized PTY' {
        $sizes.After = Invoke-DesktopWsl $measure
        $after = [regex]::Match($sizes.After, '^(\d+) (\d+)$')
        $after.Success -and [int] $after.Groups[1].Value -gt $rows -and [int] $after.Groups[2].Value -gt $cols
    }
    $sizes | ConvertTo-Json | Set-Content (Join-Path $Output 'pty-resize.json') -Encoding UTF8
    Get-WindowText 'resized' | Out-Null
}
