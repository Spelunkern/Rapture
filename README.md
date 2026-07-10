# Rapture Project

An open source Shaiya emulator targeting the 5.4 client, designed to be a
community project to replace old server files with a modern, functional,
and secure alternative.

Rapture is written in Rust and runs on Windows, with a Linux build to come
in the near future. Currently, Rapture uses SQL Server just like the
original files — you can run it using any standard Shaiya database.

**New here?** See [docs/INSTALLATION.md](docs/INSTALLATION.md) for the full
installation and usage guide (prerequisites, database setup, running the
servers, GM commands, trade/experience). This README is a quick reference.

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

Build:

```powershell
powershell -ExecutionPolicy Bypass -File tools\build-rust.ps1
```

Open the Rapture panel:

```powershell
powershell -ExecutionPolicy Bypass -File tools\run-rust-panel.ps1
```

The Rapture Emulator panel starts and stops the headless Login and World processes.

## Database

Rapture uses the original SQL Server databases:

- `PS_UserData.dbo.Users_Master` for login accounts.
- `PS_GameData.dbo.UserMaxGrow` for account faction and max mode.
- `PS_GameData.dbo.Chars` for character selection, creation, and position saves.
- `PS_GameDefs` for static definitions (items, mobs, exp/status tables, drops).

SQL Server must already be running locally with TCP/IP enabled. By default,
Rapture connects to `localhost:1433` with Windows authentication.

Optional overrides:

```powershell
$env:RAPTURE_SQLSERVER_HOST = "localhost"
$env:RAPTURE_SQLSERVER_PORT = "1433"
$env:RAPTURE_SQLSERVER_POOL_SIZE = "16"
```

## Layout

- `rapture_login`: headless Login server, TCP `127.0.0.1:30800`.
- `rapture_world`: headless World server, TCP `127.0.0.1:30810`.
- `rapture_common`: packet and crypto helpers.
- `rapture_panel`: graphical control panel ("Rapture Emulator").
The graphical control panel starts, stops, and monitors the headless Rust
processes instead of embedding server logic directly.

## References

- `C:\Shaiya-Aries`: primary reference for the target client packet layouts.
- `C:\SH.Imgeneus`: secondary architecture/protocol reference.

## License

BSD 3-Clause. See [LICENSE](LICENSE).
