$ErrorActionPreference = 'Stop'

$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$repoRoot = Split-Path -Parent $PSScriptRoot

Set-Location $repoRoot

$loginExe = Join-Path $repoRoot 'target\release\rapture_login.exe'
$worldExe = Join-Path $repoRoot 'target\release\rapture_world.exe'
$panelExe = Join-Path $repoRoot 'target\release\rapture_panel.exe'

if (!(Test-Path $loginExe) -or !(Test-Path $worldExe) -or !(Test-Path $panelExe)) {
    cargo build --workspace --release
}

Get-Process rapture_login, rapture_world -ErrorAction SilentlyContinue | Stop-Process -Force

Start-Process -FilePath $loginExe -WorkingDirectory $repoRoot -WindowStyle Hidden
Start-Process -FilePath $worldExe -WorkingDirectory $repoRoot -WindowStyle Hidden
Start-Process -FilePath $panelExe -WorkingDirectory $repoRoot

Start-Sleep -Milliseconds 700

Get-Process rapture_login, rapture_world, rapture_panel -ErrorAction SilentlyContinue |
    Select-Object ProcessName, Id, StartTime, Path
