$ErrorActionPreference = 'Stop'

$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$repoRoot = Split-Path -Parent $PSScriptRoot

Get-Process rapture_panel -ErrorAction SilentlyContinue | Stop-Process -Force

Set-Location $repoRoot
cargo build --workspace --release

Start-Process -FilePath (Join-Path $repoRoot 'target\release\rapture_panel.exe') -WorkingDirectory $repoRoot
