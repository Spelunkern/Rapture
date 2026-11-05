# Installation & Usage Guide

This guide covers everything needed to get Rapture running from a fresh
clone: prerequisites, build, database setup, starting the servers, and
day-to-day usage (GM commands, trade, experience/leveling).

## 1. Prerequisites

Rapture runs on **both Windows and Linux** from the same Go code.

- **Go 1.27 or newer.**
- **Docker** and **Python 3** to run `tools/sqlserver.py`, which hosts
  Rapture's own SQL Server 2022 container.
- **The original EP4.5 database backups** (`PS_UserData`, `PS_GameData`,
  `PS_GameDefs`, ... as found in `PSM_Data`).
- **The 5.4 client** (`Game.exe`), configured to connect to your server's
  IP (see [Client Setup](#5-client-setup)).

> The world server is still being ported from the former Rust code; until
> it lands, the client can log in and see the server list but cannot enter
> the game.

## 2. Clone & Build

```sh
git clone <this-repo-url>
cd Rapture
tools/build.sh                                            # Linux
powershell -ExecutionPolicy Bypass -File tools\build.ps1  # Windows
```

This builds `build/rapture_login`, `build/rapture_world` (when present) and
`build/rapture_console` (`.exe` on Windows).

## 3. Database

```sh
python3 tools/sqlserver.py setup   # optionally --backups /path/to/PSM_Data
```

`setup` creates the `rapture-sqlserver` container on `127.0.0.1:14346`,
restores every `PS_*`/`OMG_*` backup (read-only mount, `DBCC CHECKDB` after
each), creates the `rapture` SQL login with its password in
`.local/runtime.env`, installs `master.dbo.fn_md5` (used by the original
login procedure) and the monthly `PS_UserLog_YYYYMM` databases. Re-running it
never replaces an existing database or credential. The servers read
`.local/runtime.env` automatically; `RAPTURE_DB_HOST`, `RAPTURE_DB_PORT`,
`RAPTURE_DB_USER` and `RAPTURE_DB_PASSWORD` override it.

Rapture uses the databases exactly as restored (schema and data) and goes
through the original stored procedures, e.g.
`PS_UserData.dbo.usp_Try_GameLogin_Taiwan` for login.

### Accounts

The EP4.5 backups contain no accounts. Rapture authenticates against
existing `PS_UserData.dbo.Users_Master` rows (`UserID`, `Pw`, `Leave = 0`).
The login procedure reads `Status`: `0` is a regular account, `16`, `32`,
`48`, `64` and `80` are the admin tiers 1 to 5. Example:

```sql
INSERT PS_UserData.dbo.Users_Master
    (UserUID, UserID, Pw, Admin, AdminLevel, UseQueue, Status, Leave, UserType, Point)
VALUES (1, 'admin1', 'secret', 1, 255, 0, 80, 0, 'N', 0);
```

Run SQL with `python3 tools/sqlserver.py query --database PS_UserData --file x.sql`.

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

**Manually**, from the repo root (so the relative `logs/`, `runtime/` and
`data/maps/` paths resolve):

```sh
./build/rapture_login &
./build/rapture_world &
```

Logs are written to `logs/rapture_login.log` and `logs/rapture_world.log`
(and echoed to the console).

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

- **Database connection fails**: run `python3 tools/sqlserver.py setup`
  (it starts a stopped container) and check `.local/runtime.env`.
- **Nothing spawns on a map**: check that a matching `.svmap` file exists
  under `data/maps/` for that map id.
- **Login fails**: confirm the account row exists in
  `PS_UserData.dbo.Users_Master` with `Leave = 0` and the exact `UserID`/`Pw`
  the client is sending; `logs/rapture_login.log` shows the procedure's
  status and the result code sent to the client.
- **Deeper internals / protocol notes**: see [docs/architecture.md](architecture.md)
  for the process split, and the source comments under `cmd/` and `internal/`
  (each module documents the protocol assumptions it implements against).
