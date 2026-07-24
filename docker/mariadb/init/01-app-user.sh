#!/bin/sh
# Creates the application user the emulator connects as (never root),
# scoped to just the three databases it touches. Reads the password from
# the mounted secret at container startup -- never hardcoded here, unlike
# a plain .sql file would require.
set -eu

# Read straight from the well-known secret mount paths rather than the
# _FILE env vars: those aren't reliably forwarded into initdb.d scripts by
# this image's entrypoint.
APP_PASSWORD="$(cat /run/secrets/rapture_app_password)"

mariadb -uroot -p"$(cat /run/secrets/mariadb_root_password)" <<-EOSQL
	CREATE USER IF NOT EXISTS 'rapture'@'%' IDENTIFIED BY '${APP_PASSWORD}';
	GRANT ALL PRIVILEGES ON PS_UserData.* TO 'rapture'@'%';
	GRANT ALL PRIVILEGES ON PS_GameData.* TO 'rapture'@'%';
	GRANT ALL PRIVILEGES ON PS_GameDefs.* TO 'rapture'@'%';
	FLUSH PRIVILEGES;
EOSQL
