#Requires -Version 5.1
# Real release qualification. Run ONLY in a disposable WSL test distribution.
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string] $Distribution)
$ErrorActionPreference = 'Stop'
$work = Join-Path ([IO.Path]::GetTempPath()) ('phux-wsl-' + [Guid]::NewGuid().ToString('N'))
$bin = Join-Path $work 'bin with spaces'
$socket = '/tmp/phux-windows-' + [Guid]::NewGuid().ToString('N') + '.sock'
$argvFile = $socket + '.argv.json'
$started = $false

function Assert-True([bool] $Condition, [string] $Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-McpProcess([string] $Distro, [string] $Request, [string] $ExpectedId) {
    # A real Windows MCP client launches a process and keeps stdin open while
    # waiting for a response. A finite PowerShell object pipe alone misses this.
    $process = [Diagnostics.Process]::new()
    $process.StartInfo.FileName = 'wsl.exe'
    # This is the documented Windows MCP client entrypoint: native wsl.exe,
    # not an extra PowerShell process between the client and the protocol.
    $arguments = @('--distribution', $Distro, '--exec', 'sh', '-c', 'exec ~/.local/bin/phux-mcp')
    $process.StartInfo.Arguments = ($arguments | ForEach-Object { '"' + $_ + '"' }) -join ' '
    $process.StartInfo.UseShellExecute = $false
    $process.StartInfo.RedirectStandardInput = $true
    $process.StartInfo.RedirectStandardOutput = $true
    $process.StartInfo.RedirectStandardError = $true
    $process.StartInfo.CreateNoWindow = $true
    $process.StartInfo.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $processStarted = $false
    try {
        Write-Output ('Native MCP program: ' + $process.StartInfo.FileName)
        $processStarted = $process.Start()
        Assert-True $processStarted 'Could not start Windows MCP child'
        Write-Output "Windows MCP child pid: $($process.Id)"
        $stderr = $process.StandardError.ReadToEndAsync()
        $bytes = [Text.Encoding]::UTF8.GetBytes($Request + "`n")
        $process.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
        $process.StandardInput.BaseStream.Flush()
        $response = $process.StandardOutput.ReadLineAsync()
        Assert-True ($response.Wait([TimeSpan]::FromSeconds(20))) 'MCP did not reply while stdin remained open'
        $reply = $response.Result | ConvertFrom-Json
        Assert-True ($reply.id -ceq $ExpectedId) 'Native Windows MCP process changed the request id'
        Assert-True ($reply.result.serverInfo.name -eq 'phux') 'Native Windows MCP initialize failed'
        $process.StandardInput.Close()
        Assert-True ($process.WaitForExit(10000)) 'MCP child did not exit after stdin closed'
        Assert-True ($process.ExitCode -eq 0) ('MCP child failed: ' + $stderr.Result)
    } finally {
        if ($processStarted) {
            $process.StandardInput.Close()
            if (-not $process.WaitForExit(5000)) { $process.Kill() }
        }
        $process.Dispose()
    }
}

try {
    # This downloads and verifies the real published Linux release, then
    # installs the PowerShell entrypoints exactly as a Windows user does.
    . (Join-Path $PSScriptRoot 'install-windows.ps1') -Distribution $Distribution -InstallDir $bin -NoModifyPath
    $cli = Join-Path $bin 'phux.ps1'
    $mcp = Join-Path $bin 'phux-mcp.ps1'
    & $cli --version
    Assert-True ($LASTEXITCODE -eq 0) 'CLI version failed'
    & $mcp --version
    Assert-True ($LASTEXITCODE -eq 0) 'MCP version failed'

    & $cli --socket $socket server --ensure
    Assert-True ($LASTEXITCODE -eq 0) 'WSL server failed to start'
    $started = $true
    $status = (& $cli --socket $socket status --json) -join "`n"
    Write-Output $status
    Assert-True (($status | ConvertFrom-Json).running) 'Server is not running'

    # An actual pane receives exact argv, including empty strings, Unicode,
    # quotes, shell metacharacters, line breaks and Windows-shaped paths.
    $values = @('', 'two words', 'single''quote', 'double"quote', 'back\slash\',
        '$HOME; touch /tmp/PHUX_INJECTION', '*?[abc]', "line`nbreak", "CR`r`nLF",
        ('"\ud83c\udf55"' | ConvertFrom-Json), '-n', 'C:\a b\')
    $python = 'import json,sys; open(sys.argv[1], "w", encoding="utf-8").write(json.dumps(sys.argv[2:], ensure_ascii=False))'
    $spawned = ((& $cli --socket $socket spawn --json --retain=60 -- python3 -c $python $argvFile @values) -join "`n") | ConvertFrom-Json
    Assert-True ($LASTEXITCODE -eq 0) 'Could not spawn the argv test pane'
    $pane = '@' + $spawned.terminal_id
    & $cli --socket $socket resource wait --timeout 20 $pane
    Assert-True ($LASTEXITCODE -eq 0) 'Argv test pane did not exit'
    $readArgs = Get-PhuxWslArguments $Distribution ('cat ' + (ConvertTo-PhuxShellLiteral $argvFile))
    $savedEncoding = [Console]::OutputEncoding
    try {
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        $received = ((& wsl.exe @readArgs) -join "`n") | ConvertFrom-Json
    } finally { [Console]::OutputEncoding = $savedEncoding }
    Assert-True ($received.Count -eq $values.Count) 'Argument count changed'
    for ($i = 0; $i -lt $values.Count; $i++) {
        Assert-True ($received[$i] -ceq $values[$i]) "Argument $i changed at the PowerShell/WSL boundary"
    }
    $checkArgs = Get-PhuxWslArguments $Distribution 'test ! -e /tmp/PHUX_INJECTION'
    & wsl.exe @checkArgs
    Assert-True ($LASTEXITCODE -eq 0) 'Shell injection occurred'

    # Pipe actual JSON-RPC through the installed stdio launcher. A Unicode id
    # proves that neither Windows' legacy codepage nor the shell ate the data.
    $id = '"\ud83c\udf55"' | ConvertFrom-Json
    $request = @{ jsonrpc = '2.0'; id = $id; method = 'initialize'; params = @{
        protocolVersion = '2025-03-26'; capabilities = @{}; clientInfo = @{ name = 'wsl-ci'; version = '1' }
    } } | ConvertTo-Json -Depth 5 -Compress
    Assert-McpProcess $Distribution $request $id
    # Diagnose the exact native stdin bytes using the SAME installed launcher
    # implementation, before asking the real MCP server to parse them.
    $probeSource = "#!/usr/bin/python3`nimport sys`nprint(sys.stdin.buffer.read().hex())`n"
    $probeEncoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($probeSource))
    $probeSetup = Get-PhuxWslArguments $Distribution ('printf %s ' + $probeEncoded + '|base64 -d > "$HOME/.local/bin/phux-wsl-bytes"; chmod 755 "$HOME/.local/bin/phux-wsl-bytes"')
    & wsl.exe @probeSetup
    $probeLauncher = Join-Path $bin 'phux-wsl-bytes.ps1'
    $probeContents = Get-PhuxLauncher $Distribution 'phux-wsl-bytes'
    $probeContents = $probeContents.Replace('$OutputEncoding = $utf8', '$OutputEncoding = $utf8; [Console]::Error.WriteLine("Encoding preambles: local={0}, input={1}, output={2}", $OutputEncoding.GetPreamble().Length, [Console]::InputEncoding.GetPreamble().Length, [Console]::OutputEncoding.GetPreamble().Length)')
    [IO.File]::WriteAllText($probeLauncher, $probeContents, [Text.UTF8Encoding]::new($false))
    $stdinBytes = ($request | & $probeLauncher) -join ''
    Write-Output ('Native stdin bytes: ' + $stdinBytes)
    $expectedBytes = [BitConverter]::ToString([Text.Encoding]::UTF8.GetBytes($request)).Replace('-', '').ToLowerInvariant()
    Assert-True ($stdinBytes.StartsWith($expectedBytes)) 'Native stdin was changed or prefixed with a BOM'
    $response = @($request | & $mcp)
    Assert-True ($LASTEXITCODE -eq 0) 'MCP stdio launcher failed'
    Write-Output ('MCP qualification response: ' + ($response -join "`n"))
    $reply = $response[0] | ConvertFrom-Json
    Assert-True ($reply.id -ceq $id) 'MCP stdin/stdout was not UTF-8'
    Assert-True ($reply.result.serverInfo.name -eq 'phux') 'MCP initialize did not reply'

    # Invalid syntax is an ordinary exit, not a terminating PowerShell error.
    $savedPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        & $cli --phux-wsl-invalid-option 2>$null | Out-Null
        $failureCode = $LASTEXITCODE
    } finally { $ErrorActionPreference = $savedPreference }
    Assert-True ($failureCode -eq 2) 'CLI exit code was not propagated'
    Write-Output "Real Windows/WSL qualification passed under PowerShell $($PSVersionTable.PSVersion)."
} finally {
    if ($started) { & $cli --socket $socket kill --server }
    if (Get-Command Get-PhuxWslArguments -ErrorAction SilentlyContinue) {
        $cleanup = Get-PhuxWslArguments $Distribution ('rm -f "$HOME/.local/bin/phux-wsl-bytes" ' + (ConvertTo-PhuxShellLiteral $argvFile))
        & wsl.exe @cleanup
    }
    if (Test-Path $work) { Remove-Item -LiteralPath $work -Recurse -Force }
}
