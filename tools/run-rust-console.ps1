$ErrorActionPreference = 'Stop'

$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$repoRoot = Split-Path -Parent $PSScriptRoot

Set-Location $repoRoot
cargo build --workspace --release

# Runs attached to this same window -- this terminal IS the console (type
# "start server" / "shutdown server" at its prompt), not a detached process.
& (Join-Path $repoRoot 'target\release\rapture_console.exe')
