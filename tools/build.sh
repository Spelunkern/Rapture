#!/bin/sh
# Builds rapture_login, rapture_world and rapture_console into build/.
set -e
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
if ! command -v go >/dev/null 2>&1; then
    for candidate in "$HOME"/.local/opt/go-*/bin /usr/local/go/bin; do
        [ -x "$candidate/go" ] && PATH="$candidate:$PATH" && break
    done
fi
go build -o build/ ./cmd/...
