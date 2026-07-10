$ErrorActionPreference = 'Stop'

$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$repoRoot = Split-Path -Parent $PSScriptRoot

Get-Process rapture_login, rapture_world -ErrorAction SilentlyContinue | Stop-Process -Force

Set-Location $repoRoot
cargo build --workspace --release

Start-Process -FilePath (Join-Path $repoRoot 'target\release\rapture_login.exe') -WorkingDirectory $repoRoot -WindowStyle Hidden
Start-Process -FilePath (Join-Path $repoRoot 'target\release\rapture_world.exe') -WorkingDirectory $repoRoot -WindowStyle Hidden

Start-Sleep -Milliseconds 500
Get-Process rapture_login, rapture_world -ErrorAction SilentlyContinue |
    Select-Object ProcessName, Id, StartTime, Path
