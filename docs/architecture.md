# Architecture

The emulator is split into headless server cores plus a separate graphical
control panel.

## Processes

- `rapture_panel.exe`: graphical operator UI ("Rapture Emulator").
- `rapture_login.exe`: login/authentication TCP server.
- `rapture_world.exe`: game/world TCP server.

The control panel owns process lifecycle and log viewing. The server cores own
protocol, state, database access, and gameplay.

## First Milestone

1. Accept a TCP connection from `Game.exe` on `127.0.0.1:30800`.
2. Send/validate the login handshake.
3. Decode `0xA102` login request using Aries layouts.
4. Return login success and server list.
5. Observe the client connecting to world on `127.0.0.1:30810`.
