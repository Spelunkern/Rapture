# Builds rapture_login, rapture_world and rapture_console into build\, then
# the rapture_switch window, which needs cgo (gcc from MSYS2/TDM-GCC on
# PATH); it is skipped with a warning if that is missing.
$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)
go build -o build\ ./cmd/rapture_login ./cmd/rapture_world ./cmd/rapture_console
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go build -ldflags "-H windowsgui" -o build\ ./cmd/rapture_switch
if ($LASTEXITCODE -ne 0) {
    Write-Warning "rapture_switch not built (needs gcc for cgo); the console still works"
}
