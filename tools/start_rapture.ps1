$ErrorActionPreference = 'Stop'

$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$repoRoot = Split-Path -Parent $PSScriptRoot

Set-Location $repoRoot

$consoleExe = Join-Path $repoRoot 'target\release\rapture_console.exe'

if (!(Test-Path $consoleExe)) {
    cargo build --workspace --release
}

# This IS the app now: a single terminal window with a "start server" /
# "shutdown server" prompt (see rapture_console). Nothing is auto-started
# here -- type "start server" once the prompt is up.
& $consoleExe
