#!/bin/sh
# Opens the Rapture Console ("start server" / "shutdown server"), building
# the binaries first if needed.
set -e
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
[ -x build/rapture_console ] || "$repo_root/tools/build.sh"
exec "$repo_root/build/rapture_console"
