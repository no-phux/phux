#Requires -Version 5.1
# Real CF_HDROP and CF_DIB clipboard data must become coordinator-owned files.
if ($MyInvocation.InvocationName -ne '.') { throw 'Dot-source from the disposable Windows smoke harness' }

function Receive-ClipboardUpload([ValidateSet('file_path', 'image_path')][string] $Variable, [ValidateSet('txt', 'bmp')][string] $Extension) {
    [PhuxDesktopGui]::Text($Variable + '=')
    Send-ControlShift 86
    $received = @{ Path = $null }
    Wait-Desktop "native clipboard upload for $Variable" {
        # Default snapshots add a box around physical rows. Unwrap through the
        # CLI's logical-row projection so a long uploaded path stays byte-exact.
        $text = Invoke-SmokeCli @('snapshot', '--unwrap', $session)
        $match = [regex]::Match($text, [regex]::Escape($Variable + '=') + '(?<path>/[A-Za-z0-9._/-]+\.' + $Extension + ')')
        if (-not $match.Success) { return $false }
        $received.Path = $match.Groups['path'].Value
        return $true
    }
    if ($received.Path.StartsWith('/mnt/')) { throw 'Clipboard pasted a Windows mount, not an uploaded coordinator-owned file' }
    [PhuxDesktopGui]::Press(13)
    return $received.Path
}

function Test-NativeClipboardUploads {
    $file = Join-Path $work 'clipboard.txt'
    [IO.File]::WriteAllText($file, 'PHUXFILEDATA', [Text.UTF8Encoding]::new($false))
    $files = New-Object System.Collections.Specialized.StringCollection
    $files.Add($file) | Out-Null
    [Windows.Forms.Clipboard]::SetFileDropList($files)
    $filePath = Receive-ClipboardUpload 'file_path' 'txt'
    $expected = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLowerInvariant()
    $actual = (Invoke-DesktopWsl ('sha256sum -- ' + (ConvertTo-DesktopShellLiteral $filePath))) -split '\s+'
    if ($actual[0] -cne $expected) { throw 'Native clipboard file bytes changed during WSL upload' }
    Send-Shell (Get-MarkerCommand 'CLIPFILE')
    Assert-Rendered 'clipboard-file' 'PHUXCLIPFILE'

    $bitmap = [Drawing.Bitmap]::new(8, 8)
    try {
        for ($y = 0; $y -lt 8; $y++) {
            for ($x = 0; $x -lt 8; $x++) {
                $bitmap.SetPixel($x, $y, [Drawing.Color]::FromArgb(255, 23 + $x, 67 + $y, 101 + $x + $y))
            }
        }
        # SetImage supplies the ordinary Windows DIB/bitmap representation,
        # not an application-private PNG format chosen to make this test pass.
        [Windows.Forms.Clipboard]::SetImage($bitmap)
        $imagePath = Receive-ClipboardUpload 'image_path' 'bmp'
    } finally { $bitmap.Dispose() }
    $check = @'
import pathlib, struct, sys
data = pathlib.Path(sys.argv[1]).read_bytes()
assert len(data) < 65536 and data[:2] == b"BM", "not a bounded clipboard BMP"
width, height, planes, bits = struct.unpack_from("<iiHH", data, 18)
assert (width, abs(height), planes) == (8, 8, 1) and bits in (24, 32)
start = struct.unpack_from("<I", data, 10)[0]
stride = ((width * bits + 31) // 32) * 4
for y in range(8):
    for x in range(8):
        row = y if height < 0 else 7 - y
        at = start + row * stride + x * (bits // 8)
        assert data[at:at + 3] == bytes((101 + x + y, 67 + y, 23 + x)), "clipboard pixels changed"
print("PHUXIMAGEOK")
'@
    $script = 'python3 - ' + (ConvertTo-DesktopShellLiteral $imagePath) + " <<'PHUX_IMAGE_CHECK'`n" + $check + "`nPHUX_IMAGE_CHECK"
    if ((Invoke-DesktopWsl $script) -cne 'PHUXIMAGEOK') { throw 'Native clipboard image did not reach the WSL filesystem intact' }
    Send-Shell (Get-MarkerCommand 'CLIPIMAGE')
    Assert-Rendered 'clipboard-image' 'PHUXCLIPIMAGE'
    @{ file = $filePath; sha256 = $expected; image = $imagePath; width = 8; height = 8; pixels = 64 } | ConvertTo-Json | Set-Content (Join-Path $Output 'clipboard-uploads.json') -Encoding UTF8
}
