# Rapture Project

<img src="docs/images/rapture.png" alt="Rapture" width="320" />

An open source Shaiya emulator targeting the 5.4 client, designed to be a
community project to replace old server files with a modern, functional,
and secure alternative.

Rapture is written in Rust and runs on both Windows and Linux. Rapture uses
SQLite for storage — zero setup, no external database server to install or
configure.

**New here?** See [docs/INSTALLATION.md](docs/INSTALLATION.md) for the full
installation and usage guide (Windows and Linux, prerequisites, database,
running the servers, GM commands, trade/experience). This README is a quick
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

## Rust

Build (Windows):

```powershell
powershell -ExecutionPolicy Bypass -File tools\build-rust.ps1
```

Build (Linux):

```sh
tools/build-rust.sh
```

Open the Rapture console — Windows: `tools\run-rust-console.ps1` (or
double-click `Start Rapture.bat` at the repo root); Linux:
`tools/run-rust-console.sh` (or `./Start\ Rapture.sh`).

The Rapture Console starts and stops the headless Login and World processes
(`start server` / `shutdown server`).

## Database

Rapture stores everything in two SQLite files under `data/`, tracked
directly in this repo as the canonical, ready-to-run data:

- `gamedata.db` — static definitions (items, mobs, skills, exp table, item
  mall catalog). Read-only at runtime.
- `world.db` — live server state (accounts, characters, inventory). Back
  this one up if you want to keep your own progress.

No external database server, no setup, no schema creation at runtime —
the server only ever opens these files as they are and fails with a clear
error if either is missing or doesn't match the expected schema. Override
the location with the `RAPTURE_DATA_DIR` env var if you don't want
`./data`.

## Layout

- `rapture_login`: headless Login server, TCP `127.0.0.1:30800`.
- `rapture_world`: headless World server, TCP `127.0.0.1:30810`.
- `rapture_common`: packet and crypto helpers.
- `rapture_console`: terminal console ("Rapture Console").
The console starts and stops the headless Rust processes (`start server` /
`shutdown server`) instead of embedding server logic directly.

## References

- Client/server protocol reference material for the target 5.4 client (not included in this repo).
- Imgeneus: an open source Shaiya emulator, used as a secondary architecture/protocol reference.

## License

BSD 3-Clause. See [LICENSE](LICENSE).

## Current Progress

A quick summary of what's implemented so far:

- **Login & world servers**: TCP login flow, character select/creation, map
  entry, and session handling against the SQLite-backed schema.
- **Spatial visibility**: entity updates (movement, combat, spawns) are
  filtered by range instead of broadcasting the whole map; chat stays
  whole-map by design.
- **Combat & mobs**: auto-attack (both directions — players damage mobs and
  mobs fight back, with passive/aggressive AI and aggro), targeting, mob
  spawn/respawn/roam sweeps, NPC patrol sweeps, drop tables, player death.
- **Passive regen**: 5% of max HP/MP/SP restored every 10 seconds while
  alive.
- **Trade**: full player-to-player trade (request, offer items/gold, ready/
  confirm, atomic completion), verified against real client packet captures.
- **Experience & leveling**: EXP gain and level-ups driven by `ExpDefs`;
  base HP/MP/SP per level is a flat, hardcoded curve (not DB-driven). Core
  stats (STR/DEX/REC/INT/LUC/WIS) do not grow automatically with level,
  matching the intended design.
- **GM commands**: item spawning, teleport (map/local), mob/NPC spawn and
  clear, stat editing, GM visibility toggle, inventory/equipment clearing —
  see [docs/INSTALLATION.md](docs/INSTALLATION.md) for the full list.
- **Codebase**: modularized by system (`combat.rs`, `movement.rs`,
  `sweeps.rs`, `commands.rs`, `trade.rs`, `inventory.rs`, `skills.rs`,
  `chat.rs`) instead of one large file, to keep future contributions easier
  to review.
