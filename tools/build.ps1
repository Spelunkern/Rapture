# Builds rapture_login, rapture_world and rapture_console into build\.
$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)
go build -o build\ ./cmd/...
