# Installation & Usage Guide

This guide covers everything needed to get Rapture running from a fresh
clone: prerequisites, build, database setup, starting the servers, and
day-to-day usage (chat commands, trade, experience/leveling).

## 1. Prerequisites

Rapture runs on **both Windows and Linux** from the same Go code.

- **Go 1.27 or newer.**
- **Docker** and **Python 3** to run `tools/sqlserver.py`, which hosts
  Rapture's own SQL Server 2022 container.
- **The original EP4.5 database backups** (`PS_UserData`, `PS_GameData`,
  `PS_GameDefs`, ... as found in `PSM_Data`).
- **The 5.4 client** (`Game.exe`), configured to connect to your server's
  IP (see [Client Setup](#5-client-setup)).

## 2. Clone & Build

```sh
git clone <this-repo-url>
cd Rapture
tools/build.sh                                            # Linux
powershell -ExecutionPolicy Bypass -File tools\build.ps1  # Windows
```

This builds `build/rapture_login`, `build/rapture_world` and
`build/rapture_console` (`.exe` on Windows), plus the `build/rapture_switch`
window when a C compiler is available (Fyne needs cgo: gcc and the
OpenGL/X11 or Wayland headers on Linux, MSYS2/TDM-GCC on Windows).

## 3. Database

```sh
python3 tools/sqlserver.py setup   # optionally --backups /path/to/PSM_Data
```

`setup` creates the `rapture-sqlserver` container on `127.0.0.1:14346`,
restores every `PS_*`/`OMG_*` backup (read-only mount, `DBCC CHECKDB` after
each), creates the `rapture` SQL login with its password in
`.local/runtime.env`, installs `master.dbo.fn_md5` (used by the original
login procedure) and the monthly `PS_UserLog_YYYYMM` databases, and
disables the three `PS_GameData.dbo.Guilds` triggers that copy guilds to
`PS_UserData.dbo.CreatedGuilds` through the linked server `PS_USERDB01`
(they need MSDTC; Rapture writes the copy itself). Re-running it
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

### Item mall catalog

The client's item mall lists the products of its `Cash.SData`, and buying
sends the product code; the server looks it up in
`PS_GameDefs.dbo.ProductList`. The stock EP4.5 rows use other codes, so load
the client catalog once (the server reads it at startup):

```sh
python3 tools/sqlserver.py query --database PS_GameDefs --file tools/sql/client_cash_catalog.sql
```

Prices are item-mall points (`Users_Master.Point`).

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
stops them. `start server` first starts the `rapture-sqlserver` container
if it is stopped (after a reboot, for example) and waits until it accepts a
login; `shutdown server` stops the container too, to free its memory. A
server that exits right after starting is reported with the last line of
its log.

**One-button window:** `build/rapture_switch` opens a small window that
shows database/login/world status and has a single Start/Stop button that
does the same as the console commands, database included. The servers
run detached, so closing the window (or the console) leaves them as they
are.

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

## 6. Usage: Chat Commands

The only chat command is `/town` (or `town`): it returns you to the nearest
spawn area of your faction on the current map, and it also works while dead.
The temporary GM commands of the former Rust server (`getitem`, `cmove`,
`mmake`, `set`, ...) were not carried over; administer characters and items
through the EP4.5 databases and their procedures instead.

## 7. Usage: Trade, Combat & Experience

- **Player-to-player trade** works end to end: request a trade by clicking
  another player, offer items and/or gold, both sides ready up and confirm,
  and the trade completes atomically (validated against a fresh DB read so
  nothing is lost if an offered item became invalid mid-trade).
- **Combat is bidirectional**: players damage mobs via auto-attack/skills,
  and mobs fight back with their own basic attack. `Mobs.AI = 0` is
  passive (only retaliates once attacked); `AI >= 1` is aggressive
  (attacks any player who enters `Mobs.ChaseRange`). A character reduced to
  0 HP dies and is gated (no move/attack/cast) until revived with the
  client's resurrect button or `/town`.
- **Recovery** follows the original server: every 5 seconds, out of combat,
  a sitting character recovers 5% of its max HP/MP/SP; standing characters
  only recover through equipment and skill bonuses.
- **Experience/leveling** follows the original `ps_game.exe` rules (as
  recovered by the Eternity reconstruction): `Chars.Exp` is cumulative and
  `PS_GameDefs.dbo.ExpDefs` gives each level's total; a mob grants
  `Mobs.Exp × (1 + 0.2 × (mob level − your level + 1))`, nothing if it is
  more than 9 levels above you. Each level adds +1 to the class's main stat,
  stat points (5 normal, 7 hard, 9 ultimate) and skill points (3, 4, 5), and
  raises HP/MP/SP per `StatusDefs` (×1.1 hard, ×1.5 ultimate).
- **New characters** get the original per-class starting stats plus the
  race bonus, start at their race's `Map.ini` position, and receive the
  `BaseItemsDefs` items with the weapon already equipped.

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
