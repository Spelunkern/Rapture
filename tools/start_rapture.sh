#!/bin/sh
set -e

export PATH="$HOME/.cargo/bin:$PATH"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

login_bin="$repo_root/target/release/rapture_login"
world_bin="$repo_root/target/release/rapture_world"
panel_bin="$repo_root/target/release/rapture_panel"

if [ ! -x "$login_bin" ] || [ ! -x "$world_bin" ] || [ ! -x "$panel_bin" ]; then
    cargo build --workspace --release
fi

pkill -f 'target/release/rapture_login' 2>/dev/null || true
pkill -f 'target/release/rapture_world' 2>/dev/null || true

nohup "$login_bin" >/dev/null 2>&1 &
nohup "$world_bin" >/dev/null 2>&1 &
nohup "$panel_bin" >/dev/null 2>&1 &

sleep 0.7

pgrep -fal 'target/release/rapture_(login|world|panel)' || true
