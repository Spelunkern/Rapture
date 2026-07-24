#!/bin/sh
set -e

export PATH="$HOME/.cargo/bin:$PATH"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"

pkill -f 'target/release/rapture_login' 2>/dev/null || true
pkill -f 'target/release/rapture_world' 2>/dev/null || true

cd "$repo_root"
cargo build --workspace --release

nohup "$repo_root/target/release/rapture_login" >/dev/null 2>&1 &
nohup "$repo_root/target/release/rapture_world" >/dev/null 2>&1 &

sleep 0.5
pgrep -fal 'target/release/rapture_(login|world)' || true
