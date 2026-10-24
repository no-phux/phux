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
    $response = @($request | & $mcp)
    Assert-True ($LASTEXITCODE -eq 0) 'MCP stdio launcher failed'
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
        $cleanup = Get-PhuxWslArguments $Distribution ('rm -f ' + (ConvertTo-PhuxShellLiteral $argvFile))
        & wsl.exe @cleanup
    }
    if (Test-Path $work) { Remove-Item -LiteralPath $work -Recurse -Force }
}
