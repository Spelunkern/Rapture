#!/usr/bin/env python3
"""Manage Rapture's own SQL Server with the original EP4.5 databases.

Actions:
  start     create/start the container (backups mounted read-only)
  restore   restore every PS_*/OMG_* backup that is not restored yet
  provision create the `rapture` runtime login and .local/runtime.env
  prepare   install master.dbo.fn_md5 and this/next month's PS_UserLog databases
  setup     start + restore + provision + prepare
  query     run a T-SQL file:  query --database PS_GameData --file x.sql

Rapture never shares Eternity's container. Existing databases are never
replaced; secrets live in .local/ (ignored by git).
"""

import argparse
import calendar
import datetime
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
LOCAL = ROOT / ".local"
CONTAINER = "rapture-sqlserver"
VOLUME = "rapture-sqlserver-data"
PORT = 14346
IMAGE = "mcr.microsoft.com/mssql/server@sha256:4402d880dd4c34bfa7d8705e56a86cd6c88da80a1f6bbbe741f999e76264a090"
SQLCMD = "/opt/mssql-tools18/bin/sqlcmd"
DEFAULT_BACKUPS = Path.home() / "Desktop" / "ep4.5" / "PSM_Data"
DATABASE_NAME = re.compile(r"(?:PS|OMG)_[A-Za-z0-9_]+")


def docker(*args, **kwargs):
    return subprocess.run(["docker", *args], check=True, text=True, **kwargs)


def sql(query, database="master"):
    # The SA password stays inside the container environment.
    command = ["exec", "-i", CONTAINER, "sh", "-c", 'export SQLCMDPASSWORD="$MSSQL_SA_PASSWORD"; exec "$@"',
               "sqlcmd", SQLCMD, "-S", "localhost", "-U", "sa", "-C", "-b", "-r", "1",
               "-w", "65535", "-d", database, "-h", "-1", "-W", "-s", "\t"]
    result = docker(*command, input="SET NOCOUNT ON;\n" + query + "\nGO\n", capture_output=True)
    return result.stdout.strip()


def literal(value):
    return "N'" + str(value).replace("'", "''") + "'"


def identifier(value):
    return "[" + value.replace("]", "]]") + "]"


def databases():
    return set(sql("SELECT name FROM sys.databases WHERE database_id > 4;").split())


def container_info():
    found = subprocess.run(["docker", "inspect", CONTAINER], capture_output=True, text=True)
    if found.returncode != 0:
        return None
    info = json.loads(found.stdout)[0]
    if info["Config"].get("Labels", {}).get("com.rapture.project") != "Rapture":
        raise ValueError(f"Container name {CONTAINER} is used by another project")
    return info


def start(backups):
    LOCAL.mkdir(mode=0o700, exist_ok=True)
    env = LOCAL / "sqlserver.env"
    if not env.exists():
        content = ("ACCEPT_EULA=Y\nMSSQL_PID=Developer\nMSSQL_SA_PASSWORD=Ra!" +
                   secrets.token_urlsafe(28) + "9a\nMSSQL_MEMORY_LIMIT_MB=3072\n")
        with os.fdopen(os.open(env, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "w") as stream:
            stream.write(content)
    info = container_info()
    if info is None:
        if not backups.is_dir():
            raise ValueError(f"Backup directory not found: {backups}")
        docker("run", "-d", "--name", CONTAINER, "--label", "com.rapture.project=Rapture",
               "--env-file", str(env), "-p", f"127.0.0.1:{PORT}:1433",
               "--mount", f"type=volume,source={VOLUME},target=/var/opt/mssql",
               "--mount", f"type=bind,source={backups.resolve()},target=/backups,readonly", IMAGE,
               capture_output=True)
    elif not info["State"]["Running"]:
        docker("start", CONTAINER, capture_output=True)
    deadline = time.monotonic() + 120
    while True:
        try:
            if sql("SELECT 1;") == "1":
                break
        except subprocess.CalledProcessError:
            pass
        if time.monotonic() >= deadline:
            raise TimeoutError("SQL Server did not become ready within 120 seconds")
        time.sleep(1)
    print(f"SQL Server ready on 127.0.0.1:{PORT}")


def restore():
    info = container_info()
    if info is None:
        raise ValueError("Run `start` first")
    source = Path(next(m["Source"] for m in info["Mounts"] if m["Destination"] == "/backups"))
    existing = databases()
    for backup in sorted(source.iterdir()):
        name = backup.name
        if not backup.is_file() or not DATABASE_NAME.fullmatch(name):
            continue
        if name in existing:
            print(f"{name}: already restored (kept)")
            continue
        path = "/backups/" + name
        moves = []
        for line in sql(f"RESTORE FILELISTONLY FROM DISK = {literal(path)};").splitlines():
            columns = [part.strip() for part in line.split("\t")]
            if len(columns) < 3 or columns[2] not in ("D", "L"):
                continue
            suffix = "mdf" if columns[2] == "D" else "ldf"
            moves.append(f"MOVE {literal(columns[0])} TO {literal(f'/var/opt/mssql/data/{name}_{len(moves)}.{suffix}')}")
        if not moves:
            raise ValueError(f"No data/log files in {name}")
        sql(f"RESTORE DATABASE {identifier(name)} FROM DISK = {literal(path)} WITH {', '.join(moves)};")
        sql(f"DBCC CHECKDB ({literal(name)}) WITH NO_INFOMSGS;")
        print(f"{name}: restored, CHECKDB passed")


def provision():
    path = LOCAL / "runtime.env"
    if path.exists():
        print("Runtime login already provisioned (.local/runtime.env kept)")
        return
    if sql("SELECT COUNT(*) FROM sys.server_principals WHERE name=N'rapture';") != "0":
        raise ValueError("Login `rapture` exists without .local/runtime.env; not replacing credentials")
    password = "Ra!" + secrets.token_urlsafe(28) + "9a"
    sql("CREATE LOGIN [rapture] WITH PASSWORD=" + literal(password) + ", CHECK_POLICY=ON;")
    for name in sorted(databases()):
        if DATABASE_NAME.fullmatch(name):
            sql("CREATE USER [rapture] FOR LOGIN [rapture]; "
                "GRANT SELECT, INSERT, UPDATE, DELETE, EXECUTE ON SCHEMA::dbo TO [rapture];", name)
    content = (f"RAPTURE_DB_HOST=127.0.0.1\nRAPTURE_DB_PORT={PORT}\nRAPTURE_DB_USER=rapture\n"
               f"RAPTURE_DB_PASSWORD={password}\n")
    with os.fdopen(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "w") as stream:
        stream.write(content)
    print("Provisioned login `rapture` and .local/runtime.env")


def prepare():
    # usp_Try_GameLogin_Taiwan calls master.dbo.fn_md5, which lives outside the backups.
    sql("""IF OBJECT_ID(N'dbo.fn_md5') IS NULL
        EXEC(N'CREATE FUNCTION dbo.fn_md5(@input varchar(8000)) RETURNS varchar(32)
               AS BEGIN RETURN LOWER(CONVERT(varchar(32), HASHBYTES(''MD5'', @input), 2)); END');""")
    if sql("SELECT dbo.fn_md5('abc');") != "900150983cd24fb0d6963f7d28e17f72":
        raise ValueError("master.dbo.fn_md5 does not return standard MD5")
    sql("IF USER_ID(N'rapture') IS NULL CREATE USER [rapture] FOR LOGIN [rapture]; "
        "GRANT EXECUTE ON OBJECT::dbo.fn_md5 TO [rapture];")
    # usp_Insert_LoginLog_E writes PS_UserLog_YYYYMM.dbo.UserLogDD.
    today = datetime.datetime.strptime(sql("SELECT CONVERT(char(8), GETDATE(), 112);"), "%Y%m%d").date()
    for offset in (0, 1):
        year = today.year + (today.month - 1 + offset) // 12
        month = (today.month - 1 + offset) % 12 + 1
        database = f"PS_UserLog_{year:04d}{month:02d}"
        if sql(f"SELECT COUNT(*) FROM sys.databases WHERE name={literal(database)};") == "0":
            sql(f"CREATE DATABASE {identifier(database)};")
        statements = [f"""IF OBJECT_ID(N'dbo.UserLog{day:02d}', N'U') IS NULL
            CREATE TABLE dbo.UserLog{day:02d} (SessionID bigint NOT NULL, UserUID int NOT NULL,
                UserIP varchar(15) NOT NULL, LogType bit NOT NULL, LogTime datetime NOT NULL,
                LoginType smallint NOT NULL);""" for day in range(1, calendar.monthrange(year, month)[1] + 1)]
        statements.append("IF USER_ID(N'rapture') IS NULL CREATE USER [rapture] FOR LOGIN [rapture]; "
                          "GRANT SELECT, INSERT ON SCHEMA::dbo TO [rapture];")
        sql("\n".join(statements), database)
        print(f"{database}: login log tables ready")
    print("fn_md5 verified")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    subs = parser.add_subparsers(dest="action", required=True)
    for action in ("start", "setup"):
        sub = subs.add_parser(action)
        sub.add_argument("--backups", type=Path, default=DEFAULT_BACKUPS)
    for action in ("restore", "provision", "prepare"):
        subs.add_parser(action)
    sub = subs.add_parser("query")
    sub.add_argument("--database", default="master")
    sub.add_argument("--file", type=Path, required=True)
    args = parser.parse_args()
    if args.action in ("start", "setup"):
        start(args.backups)
    if args.action in ("restore", "setup"):
        restore()
    if args.action in ("provision", "setup"):
        provision()
    if args.action in ("prepare", "setup"):
        prepare()
    if args.action == "query":
        print(sql(args.file.read_text(), args.database))


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        print(error.stderr or str(error), file=sys.stderr)
        sys.exit(1)
