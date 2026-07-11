# Installation & Usage Guide

This guide covers everything needed to get Rapture running from a fresh
clone: prerequisites, build, database setup, starting the servers, and
day-to-day usage (GM commands, trade, experience/leveling).

## 1. Prerequisites

- **Windows** (the current binaries assume Windows paths/process handling;
  the panel in particular uses Windows-only APIs).
- **Rust toolchain**, edition 2024 (stable channel, e.g. via
  [rustup](https://rustup.rs)). Tested with Rust 1.96.
- **Microsoft SQL Server**, reachable at `localhost:1433`
  by default, with **Windows Integrated Authentication** and TCP/IP enabled.
- **An existing Shaiya database** already restored into SQL Server.
  Rapture does not create or seed this schema — it expects the original
  retail-style databases (`PS_UserData`, `PS_GameData`, `PS_GameDefs`) to
  already exist with their tables and definition data (items, mobs, exp
  tables, etc.) populated. If you don't have a copy of these databases, you
  will need to source one before Rapture is useful.
- **The 5.4 client** (`Game.exe`), configured to connect to
  `127.0.0.1` (see [Client Setup](#5-client-setup) below).

## 2. Clone & Build

```powershell
git clone <this-repo-url>
cd Rapture
powershell -ExecutionPolicy Bypass -File tools\build-rust.ps1
```

This runs `cargo build --workspace --release`. Always build in **release**
mode — debug builds are not supported/tested and the project's own tooling
assumes `target\release\*.exe`.

The workspace has four crates:

| Crate | Produces | Role |
|---|---|---|
| `rapture_common` | (library) | Shared packet/crypto/DB helpers |
| `rapture_login` | `rapture_login.exe` | Headless login/authentication server, TCP `127.0.0.1:30800` |
| `rapture_world` | `rapture_world.exe` | Headless game/world server, TCP `127.0.0.1:30810` |
| `rapture_panel` | `rapture_panel.exe` | "Rapture Emulator" GUI — starts/stops/monitors the two headless servers |

## 3. Database Configuration

By default Rapture connects to `localhost:1433` with Windows
authentication and no password (Integrated Security). Override with
environment variables if your SQL Server instance differs:

```powershell
$env:RAPTURE_SQLSERVER_HOST = "localhost"
$env:RAPTURE_SQLSERVER_PORT = "1433"
$env:RAPTURE_SQLSERVER_POOL_SIZE = "16"   # max pooled connections
```

Set these before starting `rapture_login.exe`/`rapture_world.exe` (or before
launching the panel, which spawns them).

### Accounts

Rapture **authenticates against existing accounts only** — it does not
provide account registration. An account must already exist as a row in
`PS_UserData.dbo.Users_Master` with:

- `UserID` / `Pw` matching the credentials you'll log in with.
- `Leave = 0` (not marked as left/banned).

To make an account a **GM** (required for the admin chat commands below),
set either flag on that row:

```sql
UPDATE PS_UserData.dbo.Users_Master
SET Admin = 1
WHERE UserID = 'your_account_name';
```

### Map data

Place original Shaiya `.svmap` files under `data\maps\`, named by map id
(e.g. `1.svmap`, `42.svmap`). `rapture_world` loads NPC and monster spawn
definitions from these at startup — without them, maps will have no
mobs/NPCs. The directory is created automatically on first run if missing.

## 4. Running the Servers

**Easiest**: double-click `Start Rapture.bat` at the repo root, or run:

```powershell
powershell -ExecutionPolicy Bypass -File tools\start_rapture.ps1
```

This builds (if needed), stops any already-running instances, and starts
`rapture_login.exe` and `rapture_world.exe` hidden in the background plus
`rapture_panel.exe` (visible GUI).

**Manually**, from the repo root (so the relative `logs\`/`runtime\`/
`data\maps\` paths resolve correctly):

```powershell
.\target\release\rapture_login.exe
.\target\release\rapture_world.exe
```

Logs are written to `logs\rapture_login.log` / `logs\rapture_world.log`
(and echoed to the console), recreated fresh each time a server starts.

## 5. Client Setup

Point your 5.4 client (`Game.exe`) at `127.0.0.1` — it will connect
to the login server on port `30800` first, then hand off to the world
server on port `30810` after server/character selection. No client-side
patch beyond the server IP is expected.

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

## 7. Usage: Trade & Experience

- **Player-to-player trade** works end to end: request a trade by clicking
  another player, offer items and/or gold, both sides ready up and confirm,
  and the trade completes atomically (validated against a fresh DB read so
  nothing is lost if an offered item became invalid mid-trade).
- **Experience/leveling** is driven by two SQL Server tables in
  `PS_GameDefs`: `ExpDefs` (exp required per level/growth stage) and
  `StatusDefs` (base HP/MP/SP per level/class). Killing mobs awards exp
  (per-mob amount from `PS_GameDefs.dbo.Mobs.Exp`); leveling up updates HP/
  MP/SP from `StatusDefs` and fully heals the character. Base stats
  (STR/DEX/REC/INT/LUC/WIS) do **not** grow automatically with level in
  this implementation — only HP/MP/SP scale with level.

## 8. Troubleshooting

- **Build looks instant / stale binary**: this project's build has been
  observed to hit filesystem mtime clock skew, which can make Cargo wrongly
  skip recompilation. If a release build finishes suspiciously fast right
  after an edit, force a clean rebuild: `cargo clean --release -p <crate>`
  then rebuild.
- **Nothing spawns on a map**: check that a matching `.svmap` file exists
  under `data\maps\` for that map id.
- **Login fails**: confirm the account row exists in
  `PS_UserData.dbo.Users_Master` with `Leave = 0` and the exact
  `UserID`/`Pw` the client is sending.
- **Deeper internals / protocol notes**: see [docs/architecture.md](architecture.md)
  for the process split, and the source comments in `crates/rapture_world/src/`
  (each module documents the protocol assumptions it implements against).
