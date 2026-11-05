# Opens the Rapture Console ("start server" / "shutdown server"), building
# the binaries first if needed.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
if (-not (Test-Path "build\rapture_console.exe")) { & "$PSScriptRoot\build.ps1" }
& "build\rapture_console.exe"
