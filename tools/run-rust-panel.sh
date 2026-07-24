#!/bin/sh
set -e

export PATH="$HOME/.cargo/bin:$PATH"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"

pkill -f 'target/release/rapture_panel' 2>/dev/null || true

cd "$repo_root"
cargo build --workspace --release

nohup "$repo_root/target/release/rapture_panel" >/dev/null 2>&1 &
