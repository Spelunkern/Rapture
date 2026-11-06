# Rapture Project

<img src="docs/images/rapture.png" alt="Rapture" width="320" />

An open source Shaiya emulator targeting the 5.4 client, designed to be a
community project to replace old server files with a modern, functional,
and secure alternative.

Rapture is written in Go and runs on both Windows and Linux. It stores data
in SQL Server, using the original EP4.5 databases (`PS_UserData`,
`PS_GameData`, `PS_GameDefs`, ...) and their stored procedures.

**Status:** the Rust implementation was replaced by Go in November 2026.
Login and world both run on SQL Server with the EP4.5 databases.

**New here?** See [docs/INSTALLATION.md](docs/INSTALLATION.md) for the full
installation and usage guide (Windows and Linux, prerequisites, database,
running the servers, chat commands, trade/experience). This README is a quick
reference.

## Goals

- A complete emulator to run Shaiya clients, starting on 5.4.
- A more secure and stable server base, with no bugs and better performance.
- Solve several decades of game design flaws.
- More flexibility about what database to use, and a better database design.
- Create and maintain features easily.
- Be able to run Shaiya on Linux at 100%, and standardize the Shaiya server
  backend on Linux.
- Much more.

## Community Goals

- To provide individuals the tools and knowledge to search for valuable
  game information to be used on the project.
- Keep research easy and simple to understand, to encourage non-technical
  users to contribute.

## Build

Install Go 1.27 or newer, then:

```sh
tools/build.sh          # Linux
```

```powershell
powershell -ExecutionPolicy Bypass -File tools\build.ps1   # Windows
```

Binaries land in `build/`. Open the Rapture Console with `./Start\ Rapture.sh`
(Linux) or `Start Rapture.bat` (Windows); it starts and stops the headless
login and world servers (`start server` / `shutdown server`). For a
one-button window instead, run `build/rapture_switch`.

## Database

`tools/sqlserver.py setup` creates Rapture's own SQL Server container
(`rapture-sqlserver`, `127.0.0.1:14346`), restores the original EP4.5
backups read-only from `~/Desktop/ep4.5/PSM_Data` (override with
`--backups`), provisions the `rapture` login and writes its credentials to
`.local/runtime.env` (ignored by git). Existing databases are never
replaced. Requires Docker.

## Layout

- `cmd/rapture_login`: login server, TCP `127.0.0.1:30800`.
- `cmd/rapture_world`: world server, TCP `127.0.0.1:30810`.
- `internal/world`: the game itself (maps, entities, combat, skills, items,
  trade, item mall, the per-connection session).
- `cmd/rapture_console`: terminal console that starts and stops both.
- `cmd/rapture_switch`: one-button window (Fyne) doing the same.
- `internal/control`: the shared start/stop logic.
- `internal/`: packets, crypto, logging, configuration, the SQL Server
  access layer (`internal/db`) and the login-to-world session handoff.
- `tools/sqlserver.py`: Rapture's own SQL Server with the EP4.5 databases.

Override listen addresses and the address clients are sent to with
`RAPTURE_LOGIN_LISTEN`, `RAPTURE_WORLD_LISTEN` and `RAPTURE_PUBLIC_IP`.

## References

- Client/server protocol reference material for the target 5.4 client (not included in this repo).
- Imgeneus: an open source Shaiya emulator, used as a secondary architecture/protocol reference.

## License

BSD 3-Clause. See [LICENSE](LICENSE).

## Current Progress

- **Login (Go):** RSA/AES key exchange, credentials checked by the original
  `PS_UserData.dbo.usp_Try_GameLogin_Taiwan` (including its login log and
  `UserLoginStatus` rows), the original status-to-result mapping, server list
  and the session handoff to world. Covered by crypto parity tests against
  the former Rust implementation and a SQL Server integration test.
- **World (Go):** character select/creation/deletion/rename, maps from
  `.svmap` (NPCs, mob spawns, portals, spawn areas), movement and
  visibility, mob AI (roam, aggro, chase, respawn), auto-attack and skills
  (damage, heals, buffs, DoTs, cast times), death and revive, experience,
  inventory, drops, NPC shops and gatekeepers, quests, parties and raids, warehouse, enchanting, gems,
  recreation runes, mounts, player trade and the item mall (buy, gifts,
  gift box). Character, item, skill, quick-slot and buff state goes through
  the original `PS_GameData` procedures; definitions come from
  `PS_GameDefs`. The temporary GM commands were dropped; `/town` remains.
  Covered by a SQL Server integration test that plays a synthetic client.
  The Rust sources remain in git history before the Go switch.
