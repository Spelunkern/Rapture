#!/bin/sh
set -e

export PATH="$HOME/.cargo/bin:$PATH"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

console_bin="$repo_root/target/release/rapture_console"
if [ ! -x "$console_bin" ]; then
    cargo build --workspace --release
fi

# This IS the app now: a single terminal window with a "start server" /
# "shutdown server" prompt (see rapture_console). Nothing is auto-started
# here -- type "start server" once the prompt is up.
exec "$console_bin"
