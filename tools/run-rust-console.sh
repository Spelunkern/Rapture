#!/bin/sh
set -e

export PATH="$HOME/.cargo/bin:$PATH"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

console_bin="$repo_root/target/release/rapture_console"
if [ ! -x "$console_bin" ]; then
    cargo build --workspace --release
fi

# Runs in the foreground -- this terminal window IS the console (type
# "start server" / "shutdown server" at its prompt), not a background
# process to detach from.
exec "$console_bin"
