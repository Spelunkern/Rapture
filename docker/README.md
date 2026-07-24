# Docker setup (dev/testing only)

This folder holds two exploratory database backends, both run via Docker
Engine inside WSL2 (Debian) on this machine -- **not Docker Desktop**. Rapture's
normal day-to-day local setup is the native SQL Server Express install with
Windows Integrated Authentication (no `config.ini` needed for that). Everything
here is optional, parallel infrastructure for testing SQL Server Authentication,
running against a containerized DB, and validating the Linux build.

## Prerequisites

- Docker Engine installed inside a WSL2 distro (see this repo's session notes
  for the install steps used -- `apt` from Docker's official repo, not
  Docker Desktop).
- Run all `docker compose` commands from this directory, inside WSL:
  ```sh
  cd /mnt/c/Rapture/docker   # adjust if cloned elsewhere
  docker compose up -d           # both mariadb and mssql
  docker compose up -d mssql      # just SQL Server
  docker compose up -d mariadb    # just MariaDB
  docker compose down             # stop everything (add -v to also wipe data)
  ```

## Services

### `rapture_mssql` -- SQL Server 2022 Developer Edition

- Same engine as production (SQL Server), just Developer Edition (free,
  full Enterprise features, **non-production license**) running on Linux
  inside the container.
- Host port **14330** (deliberately not 1433, so this can run alongside the
  native SQL Server Express instance without a conflict).
- SA password: `mssql/sa_password.txt` (generated per-machine, gitignored).
- App login `rapture` (SQL Server Authentication, scoped to just the 3
  project databases via `db_owner` on each -- not `sa`): password in
  `mssql/app_password.txt` (gitignored).
- Data restored directly from the `.bak` backups at
  `C:\Backups\Rapture\sql\<date>\` (mounted read-only into the container at
  `/var/opt/mssql/backup`) -- same SQL Server engine on both sides, so this
  is a plain `RESTORE DATABASE`, no schema translation needed (unlike the
  MariaDB path below).
- **Known gotcha on this machine**: `Host=localhost` resolves to IPv6
  (`::1`) first and the TLS handshake against this container hangs there.
  Always use `Host=127.0.0.1` explicitly (in `config.ini`, `sqlcmd`, etc.)
  when pointing at this container.
- To point the emulator at it: copy `config.ini.example` (repo root) to
  `config.ini`, set `Host=127.0.0.1`, `Port=14330`, `User=rapture`,
  `Password=<contents of mssql/app_password.txt>`. Remove/rename
  `config.ini` to go back to the native instance + Windows Integrated Auth.

### `rapture_mariadb` -- MariaDB 11.4

- Exploratory alternative engine, **not currently used by the app**
  (`rapture_common/src/db.rs` only ever speaks SQL Server/tiberius -- this
  container's data is not wired to anything).
- Host port 3306. Root password: `mariadb/root_password.txt` (gitignored).
  App user `rapture` (scoped to the 3 databases): `mariadb/app_password.txt`
  (gitignored).
- Schema + data migrated from SQL Server via `migrate/migrate_to_mariadb.py`
  (a from-scratch DDL translation + row copy, since MariaDB is a different
  engine -- re-run it any time to re-sync from a fresh SQL Server backup;
  it drops and recreates every table first).

## Secrets

Every `*_password.txt` file under `mariadb/` and `mssql/` is generated
per-machine and gitignored (`docker/mariadb/*_password.txt`,
`docker/mssql/*_password.txt` in the root `.gitignore`) -- never commit a
filled-in one. Re-run the password-generation step (see session notes) if
you need to stand this up fresh on another machine.

## Why this exists

Two things prompted this setup: (1) wanting to compare SQL Server
Authentication against Windows Integrated Authentication without touching
the native install, and (2) validating that Rapture can build and run on
Linux -- SQL Server itself already has a native Linux distribution
(confirmed by this `rapture_mssql` container running it), so the emulator's
own Linux port only needed the auth-method and `rapture_panel`
process-management fixes already made in the Rust source (see
`AuthMethod` handling in `rapture_common/src/db.rs`).
