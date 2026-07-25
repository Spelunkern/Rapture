# Architecture

The emulator is split into headless server cores plus a separate terminal
console.

## Processes

- `rapture_console`: terminal operator console ("Rapture Console") --
  `start server` / `shutdown server`.
- `rapture_login`: login/authentication TCP server.
- `rapture_world`: game/world TCP server.

(`.exe` on Windows, no extension on Linux -- otherwise identical binaries.)

The console owns process lifecycle. The server cores own protocol, state,
database access, and gameplay.

## First Milestone

1. Accept a TCP connection from `Game.exe` on `127.0.0.1:30800`.
2. Send/validate the login handshake.
3. Decode `0xA102` login request using Aries layouts.
4. Return login success and server list.
5. Observe the client connecting to world on `127.0.0.1:30810`.
