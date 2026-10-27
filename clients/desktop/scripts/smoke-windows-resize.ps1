#Requires -Version 5.1
if ($MyInvocation.InvocationName -ne '.') { throw 'Dot-source from the disposable Windows smoke harness' }

function Test-NativeResize {
    $beforeFile = ConvertTo-DesktopShellLiteral "$linuxRoot/size-before"
    $afterFile = ConvertTo-DesktopShellLiteral "$linuxRoot/size-after"
    Send-Shell ("stty size > $beforeFile")
    $sizes = @{ Before = ''; After = '' }
    Wait-Desktop 'initial PTY size' {
        $sizes.Before = Invoke-DesktopWsl ("cat $beforeFile")
        $sizes.Before -match '^\d+ \d+$'
    }
    $before = [regex]::Match($sizes.Before, '^(\d+) (\d+)$')
    $rows = [int] $before.Groups[1].Value
    $cols = [int] $before.Groups[2].Value
    $screen = [Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    if (-not [PhuxDesktopGui]::SetWindowPos($desktop.MainWindowHandle, [IntPtr]::Zero, 0, 0, $screen.Width - 10, $screen.Height - 10, 0x0040)) { throw 'Native window resize failed' }
    # Sample the real PTY until the asynchronous native fit reaches it. Send
    # this bounded command once; scrollback/OCR cannot substitute for stty.
    Send-Shell ("i=0; while [ " + '"$i"' + " -lt 50 ]; do set -- `$(stty size); if [ " + '"$1"' + " -gt $rows ] && [ " + '"$2"' + " -gt $cols ]; then printf '%s %s\n' " + '"$1" "$2"' + " > $afterFile; break; fi; i=`$((i+1)); sleep 0.2; done")
    Wait-Desktop 'resized PTY' {
        $sizes.After = Invoke-DesktopWsl ("cat $afterFile")
        $after = [regex]::Match($sizes.After, '^(\d+) (\d+)$')
        $after.Success -and [int] $after.Groups[1].Value -gt $rows -and [int] $after.Groups[2].Value -gt $cols
    }
    $sizes | ConvertTo-Json | Set-Content (Join-Path $Output 'pty-resize.json') -Encoding UTF8
    Get-WindowText 'resized' | Out-Null
}
