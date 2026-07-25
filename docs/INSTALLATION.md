# Installation & Usage Guide

This guide covers everything needed to get Rapture running from a fresh
clone: prerequisites, build, database setup, starting the servers, and
day-to-day usage (GM commands, trade, experience/leveling).

## 1. Prerequisites

Rapture runs on **both Windows and Linux** — the binaries are the same
codebase, no platform-specific feature gaps between them.

- **Rust toolchain**, edition 2024 (stable channel, e.g. via
  [rustup](https://rustup.rs)). Tested with Rust 1.96/1.97.
- **No external database server** — Rapture stores everything in two
  SQLite files under `data/`, shipped directly in this repo. See
  [Database](#3-database) below.
- **The 5.4 client** (`Game.exe`), configured to connect to your server's
  IP (see [Client Setup](#5-client-setup) below). The client itself is
  Windows-only (it's the original Shaiya game client) — a Linux-hosted
  server is for players connecting from a separate Windows machine, not
  for running the client under Linux too.

## 2. Clone & Build

**Windows** (PowerShell):

```powershell
git clone <this-repo-url>
cd Rapture
powershell -ExecutionPolicy Bypass -File tools\build-rust.ps1
```

**Linux**:

```sh
git clone <this-repo-url>
cd Rapture
chmod +x tools/*.sh "Start Rapture.sh"
tools/build-rust.sh
```

Both run `cargo build --workspace --release`. Always build in **release**
mode — debug builds are not supported/tested and the project's own tooling
assumes `target/release/*` (`.exe` suffix on Windows, none on Linux).

The workspace has four crates:

| Crate | Produces | Role |
|---|---|---|
| `rapture_common` | (library) | Shared packet/crypto/DB helpers |
| `rapture_login` | `rapture_login[.exe]` | Headless login/authentication server, TCP `127.0.0.1:30800` |
| `rapture_world` | `rapture_world[.exe]` | Headless game/world server, TCP `127.0.0.1:30810` |
| `rapture_console` | `rapture_console[.exe]` | Terminal console — `start server` / `shutdown server` for the two headless servers |

## 3. Database

Rapture stores everything in two SQLite files under `data/` (path
overridable with the `RAPTURE_DATA_DIR` env var), tracked directly in this
repo as ready-to-run canonical data — no setup step:

- `gamedata.db` — static definitions (items, mobs, skills, exp table, item
  mall catalog). Read-only at runtime.
- `world.db` — live server state (accounts, characters, inventory). Back
  this one up if you want to keep your own progress; it already ships with
  a handful of test accounts/characters to log in with immediately.

The server **never creates or migrates schema at runtime** — it only ever
opens these two files as they already are. If either is missing, or is
missing a table the server expects, startup fails immediately with a clear
error naming the file and what's wrong, instead of silently creating or
patching anything. If you need to rebuild these files from scratch (e.g.
a content update, or importing from an old SQL-Server-backed install), that
happens with separate tooling outside this repo, not with the shipped
binaries.

### Accounts

Rapture **authenticates against existing accounts only** — it does not
provide account registration. An account must already exist as a row in
`world.db`'s `Users_Master` table with:

- `UserID` / `Pw` matching the credentials you'll log in with.
- `Leave = 0` (not marked as left/banned).

To make an account a **GM** (required for the admin chat commands below),
set the `Admin` flag on that row (any SQLite client, e.g. the `sqlite3`
CLI, against `data/world.db`):

```sql
UPDATE Users_Master SET Admin = 1 WHERE UserID = 'your_account_name';
```

### Map data

Place original Shaiya `.svmap` files under `data/maps/`, named by map id
(e.g. `1.svmap`, `42.svmap`). `rapture_world` loads NPC and monster spawn
definitions from these at startup — without them, maps will have no
mobs/NPCs. The directory is created automatically on first run if missing.
This map data is proprietary and not included in this repo (see
`data/maps/README.txt`) — source your own copy from an original Shaiya
install.

## 4. Running the Servers

**Windows** — easiest: double-click `Start Rapture.bat` at the repo root,
or run:

```powershell
powershell -ExecutionPolicy Bypass -File tools\start_rapture.ps1
```

**Linux** — easiest: run `./Start\ Rapture.sh` at the repo root (or
`tools/start_rapture.sh`).

Either way this builds (if needed) and opens the console. Type
`start server` at its prompt to launch the login/world processes (hidden in
the background on Windows, or just backgrounded on Linux); `shutdown server`
stops them.

**Manually**, from the repo root (so the relative `logs/`/`runtime/`/
`data/maps/` paths resolve correctly):

```powershell
.\target\release\rapture_login.exe
.\target\release\rapture_world.exe
```

```sh
./target/release/rapture_login &
./target/release/rapture_world &
```

Logs are written to `logs/rapture_login.log` / `logs/rapture_world.log`
(and echoed to the console), recreated fresh each time a server starts.

## 5. Client Setup

Point your 5.4 client (`Game.exe`, Windows-only) at your server's IP — it
will connect to the login server on port `30800` first, then hand off to
the world server on port `30810` after server/character selection (use
`127.0.0.1` if the client runs on the same machine as the server; use the
server's LAN/public IP if connecting from elsewhere — a Linux-hosted server
is reached the same way, just make sure ports `30800`/`30810` are open). No
client-side patch beyond the server IP is expected.

## 6. Usage: GM Commands

GM commands are typed in chat like normal messages (the client strips a
leading `/`, so `/cmove` and `cmove` both work). All of them require the
account to be flagged as GM (see [Accounts](#accounts) above) except
`town`, which is a normal player command.

| Command | Syntax | Effect |
|---|---|---|
| `getitem` | `getitem <type> <typeId> [count]` | Creates an item in your inventory (default count 1) |
| `cmove` | `cmove <mapId> [x y z]` | Teleport to another map, optionally to exact coordinates (falls back to a spawn area if omitted/invalid) |
| `bmove` | `bmove <x> <y> <z>` | Teleport within the current map |
| `town` | `town` | Return to the nearest town spawn for your faction (no GM required) |
| `mmake` | `mmake <mobId> <count> [area]` | Spawn `count` mobs scattered within (25 + area) meters |
| `mera` | `mera [area]` | Remove every mob within (25 + area) meters |
| `nmake` | `nmake <type> <typeId>` | Spawn one NPC at your position |
| `nera` | `nera [area]` | Remove every NPC within (5 + area) meters |
| `mobreload` | `mobreload <mapId>` | Clear and respawn a map's `.svmap`-defined mobs |
| `npcreload` | `npcreload <mapId>` | Same, for NPCs |
| `set` | `set [charname] <level\|grow\|money\|statpoint\|skillpoint\|str\|dex\|rec\|int\|luc\|wis\|kills\|deaths> <amount>` | Set a stat/attribute on yourself or another character by name |
| `char` | `char on\|off` | Toggle GM visibility to non-admins (`off` = visible, `on` = invisible, the default) |
| `amove` | `amove <charname>` | Teleport yourself to another character's location |
| `asummon` | `asummon <charname>` | Teleport another (online) character to your location |
| `iclear` | `iclear [charname]` | Clear inventory (self or named character) |
| `eclear` | `eclear [charname]` | Clear equipped items (self or named character) |

## 7. Usage: Trade, Combat & Experience

- **Player-to-player trade** works end to end: request a trade by clicking
  another player, offer items and/or gold, both sides ready up and confirm,
  and the trade completes atomically (validated against a fresh DB read so
  nothing is lost if an offered item became invalid mid-trade).
- **Combat is bidirectional**: players damage mobs via auto-attack/skills,
  and mobs fight back with their own basic attack. `Mobs.AI = 0` is
  passive (only retaliates once attacked); `AI >= 1` is aggressive
  (attacks any player who enters `Mobs.ChaseRange`). A character reduced to
  0 HP dies and is gated (no move/attack/cast) until revived; `/town` is
  the current revive path.
- **Passive regen**: while alive, a character restores 5% of max HP/MP/SP
  every 10 seconds, unconditionally (not paused by combat).
- **Experience/leveling** is driven by `gamedata.db`'s `ExpDefs` table (exp
  required per level/growth stage). Killing mobs awards exp (per-mob amount
  from `Mobs.Exp`); leveling up sets HP/MP/SP from a flat, hardcoded
  per-level curve (not DB-driven) and fully heals the character. Base stats
  (STR/DEX/REC/INT/LUC/WIS) do **not** grow automatically with level in
  this implementation — only HP/MP/SP scale with level.

## 8. Troubleshooting

- **Build looks instant / stale binary**: this project's build has been
  observed to hit filesystem mtime clock skew, which can make Cargo wrongly
  skip recompilation. If a release build finishes suspiciously fast right
  after an edit, force a clean rebuild: `cargo clean --release -p <crate>`
  then rebuild.
- **Nothing spawns on a map**: check that a matching `.svmap` file exists
  under `data/maps/` for that map id.
- **Login fails**: confirm the account row exists in `world.db`'s
  `Users_Master` table with `Leave = 0` and the exact `UserID`/`Pw` the
  client is sending.
- **Deeper internals / protocol notes**: see [docs/architecture.md](architecture.md)
  for the process split, and the source comments in `crates/rapture_world/src/`
  (each module documents the protocol assumptions it implements against).
