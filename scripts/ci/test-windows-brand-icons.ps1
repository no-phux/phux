# Compile the exact managed Windows adapter and call its native icon API.
# This standalone workspace needs neither libghostty nor a WSL coordinator.
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$scratch = Join-Path ([IO.Path]::GetTempPath()) ('phux-brand-icons-' + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $scratch | Out-Null
try {
    Copy-Item (Join-Path $repo 'rust-toolchain.toml') $scratch
    Copy-Item (Join-Path $repo 'clients/desktop/native/Cargo.lock') $scratch
    $managed = Join-Path $repo 'clients/desktop/toolchain/gpui'
    Copy-Item (Join-Path $managed 'gpui-fast-windows') (Join-Path $scratch 'windows') -Recurse
    $core = (Join-Path $managed 'gpui-fast').Replace('\', '/')
    $fixture = (Join-Path $repo 'clients/desktop/toolchain/tests/windows-icons.rs').Replace('\', '/')
    @"
[package]
name = "phux-windows-brand-icons"
version = "0.0.0"
edition = "2024"
[workspace]
[[bin]]
name = "windows-brand-icons"
path = "$fixture"
[dependencies]
anyhow = "1"
base64 = "0.23"
serde_json = "1"
gpui = { package = "gpui-fast", version = "=0.1.12" }
gpui-windows = { package = "gpui-fast-windows", path = "windows" }
[patch.crates-io]
gpui-fast = { path = "$core" }
"@ | Set-Content (Join-Path $scratch 'Cargo.toml')
    # Resolve only the disposable fixture; reuse the managed lock's latest
    # windows-capture instead of imposing the obsolete 1.4.2 downgrade.
    cargo run --manifest-path (Join-Path $scratch 'Cargo.toml') --bin windows-brand-icons
    if ($LASTEXITCODE -ne 0) { throw "Windows icon fixture failed: $LASTEXITCODE" }
} finally {
    Remove-Item $scratch -Recurse -Force
}
