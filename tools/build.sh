#!/bin/sh
# Builds rapture_login, rapture_world and rapture_console into build/, then
# the rapture_switch window, which needs cgo (a C compiler and the OpenGL /
# X11 or Wayland development headers); it is skipped with a warning if those
# are missing.
set -e
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
if ! command -v go >/dev/null 2>&1; then
    for candidate in "$HOME"/.local/opt/go-*/bin /usr/local/go/bin; do
        [ -x "$candidate/go" ] && PATH="$candidate:$PATH" && break
    done
fi
go build -o build/ ./cmd/rapture_login ./cmd/rapture_world ./cmd/rapture_console
go build -o build/ ./cmd/rapture_switch ||
    echo "warning: rapture_switch not built (needs a C compiler and OpenGL headers); the console still works" >&2
