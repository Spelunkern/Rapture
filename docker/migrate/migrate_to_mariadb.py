#!/usr/bin/env python3
"""One-shot schema+data migration: local SQL Server (PS_UserData,
PS_GameData, PS_GameDefs) -> the MariaDB container in ./docker-compose.yml.

Re-runnable: drops and recreates every table in the MariaDB side before
loading, so running this again after a fresh SQL Server export just
re-syncs MariaDB to match. Does NOT touch SQL Server (source is read-only
throughout).

Usage: python migrate_to_mariadb.py
Requires: pyodbc, pymysql (pip install pyodbc pymysql), the MariaDB
container from docker-compose.yml already up, and the app password file
at docker/mariadb/app_password.txt.
"""

from __future__ import annotations

import pathlib
import sys

import pymysql
import pyodbc

DATABASES = ["PS_UserData", "PS_GameData", "PS_GameDefs"]
BATCH_SIZE = 2000
MARIADB_HOST = "127.0.0.1"
MARIADB_PORT = 3306
MARIADB_USER = "rapture"

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
APP_PASSWORD_FILE = SCRIPT_DIR.parent / "mariadb" / "app_password.txt"


def sql_server_connect(database: str) -> pyodbc.Connection:
    conn_str = (
        "DRIVER={ODBC Driver 17 for SQL Server};"
        "SERVER=localhost;"
        f"DATABASE={database};"
        "Trusted_Connection=yes;"
    )
    return pyodbc.connect(conn_str, autocommit=True)


def mariadb_connect(database: str) -> pymysql.connections.Connection:
    password = APP_PASSWORD_FILE.read_text().strip()
    return pymysql.connect(
        host=MARIADB_HOST,
        port=MARIADB_PORT,
        user=MARIADB_USER,
        password=password,
        database=database,
        charset="utf8mb4",
        autocommit=False,
    )


# Maps a (DATA_TYPE, CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION, NUMERIC_SCALE)
# tuple from SQL Server's INFORMATION_SCHEMA.COLUMNS to a MariaDB column type
# string. Covers every type actually observed across the three databases --
# extend here if a table uses something not listed.
def map_type(data_type: str, char_len, num_precision, num_scale) -> str:
    data_type = data_type.lower()
    if data_type in ("int",):
        return "INT"
    if data_type in ("bigint",):
        return "BIGINT"
    if data_type in ("smallint",):
        return "SMALLINT"
    if data_type in ("tinyint",):
        return "TINYINT UNSIGNED"
    if data_type in ("bit",):
        return "TINYINT(1)"
    if data_type in ("float",):
        return "DOUBLE"
    if data_type in ("real",):
        return "FLOAT"
    if data_type in ("decimal", "numeric"):
        precision = num_precision or 18
        scale = num_scale or 0
        return f"DECIMAL({precision},{scale})"
    if data_type in ("money", "smallmoney"):
        return "DECIMAL(19,4)"
    if data_type in ("datetime", "datetime2", "smalldatetime", "date"):
        return "DATETIME"
    if data_type in ("time",):
        return "TIME"
    if data_type in ("uniqueidentifier",):
        return "CHAR(36)"
    if data_type in ("varchar", "nvarchar"):
        if char_len is None or char_len == -1:
            return "TEXT"
        return f"VARCHAR({char_len})"
    if data_type in ("char", "nchar"):
        return f"CHAR({char_len})"
    if data_type in ("text", "ntext"):
        return "TEXT"
    if data_type in ("varbinary",):
        if char_len is None or char_len == -1:
            return "LONGBLOB"
        return f"VARBINARY({char_len})"
    if data_type in ("binary",):
        return f"BINARY({char_len})"
    if data_type in ("image",):
        return "LONGBLOB"
    raise ValueError(f"unmapped SQL Server type: {data_type}")


def get_tables(sql_conn: pyodbc.Connection) -> list[str]:
    cur = sql_conn.cursor()
    cur.execute(
        "SELECT TABLE_NAME FROM INFORMATION_SCHEMA.TABLES "
        "WHERE TABLE_TYPE = 'BASE TABLE' ORDER BY TABLE_NAME"
    )
    return [row.TABLE_NAME for row in cur.fetchall()]


def get_columns(sql_conn: pyodbc.Connection, table: str) -> list[dict]:
    cur = sql_conn.cursor()
    cur.execute(
        """
        SELECT c.COLUMN_NAME, c.DATA_TYPE, c.CHARACTER_MAXIMUM_LENGTH,
               c.NUMERIC_PRECISION, c.NUMERIC_SCALE, c.IS_NULLABLE,
               COLUMNPROPERTY(OBJECT_ID(c.TABLE_NAME), c.COLUMN_NAME, 'IsIdentity') AS IS_IDENTITY
        FROM INFORMATION_SCHEMA.COLUMNS c
        WHERE c.TABLE_NAME = ?
        ORDER BY c.ORDINAL_POSITION
        """,
        table,
    )
    columns = []
    for row in cur.fetchall():
        columns.append(
            {
                "name": row.COLUMN_NAME,
                "data_type": row.DATA_TYPE,
                "char_len": row.CHARACTER_MAXIMUM_LENGTH,
                "precision": row.NUMERIC_PRECISION,
                "scale": row.NUMERIC_SCALE,
                "nullable": row.IS_NULLABLE == "YES",
                "identity": bool(row.IS_IDENTITY),
            }
        )
    return columns


def get_primary_key(sql_conn: pyodbc.Connection, table: str) -> list[str]:
    cur = sql_conn.cursor()
    cur.execute(
        """
        SELECT kcu.COLUMN_NAME
        FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc
        JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu
          ON tc.CONSTRAINT_NAME = kcu.CONSTRAINT_NAME AND tc.TABLE_NAME = kcu.TABLE_NAME
        WHERE tc.TABLE_NAME = ? AND tc.CONSTRAINT_TYPE = 'PRIMARY KEY'
        ORDER BY kcu.ORDINAL_POSITION
        """,
        table,
    )
    return [row.COLUMN_NAME for row in cur.fetchall()]


def quote_ident(name: str) -> str:
    return f"`{name}`"


def build_create_table(table: str, columns: list[dict], pk: list[str]) -> str:
    lines = []
    identity_cols = []
    for col in columns:
        col_type = map_type(col["data_type"], col["char_len"], col["precision"], col["scale"])
        parts = [quote_ident(col["name"]), col_type]
        if not col["nullable"]:
            parts.append("NOT NULL")
        if col["identity"]:
            parts.append("AUTO_INCREMENT")
            identity_cols.append(col["name"])
        lines.append(" ".join(parts))
    if pk:
        lines.append(f"PRIMARY KEY ({', '.join(quote_ident(c) for c in pk)})")
    # MariaDB requires every AUTO_INCREMENT column to be the leading column
    # of some key -- add one explicitly for any identity column the table's
    # actual primary key doesn't already cover (some source tables have no
    # PK at all, or an identity column that isn't the PK).
    for identity_col in identity_cols:
        if identity_col not in pk:
            lines.append(f"KEY ({quote_ident(identity_col)})")
    body = ",\n  ".join(lines)
    return f"CREATE TABLE {quote_ident(table)} (\n  {body}\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;"


def migrate_table(sql_conn: pyodbc.Connection, maria_conn: pymysql.connections.Connection, table: str) -> tuple[int, int]:
    columns = get_columns(sql_conn, table)
    pk = get_primary_key(sql_conn, table)

    maria_cur = maria_conn.cursor()
    maria_cur.execute(f"DROP TABLE IF EXISTS {quote_ident(table)}")
    maria_cur.execute(build_create_table(table, columns, pk))

    col_names = [c["name"] for c in columns]
    select_cols = ", ".join(quote_ident_sqlserver(c) for c in col_names)
    sql_cur = sql_conn.cursor()
    sql_cur.execute(f"SELECT {select_cols} FROM {quote_ident_sqlserver(table)}")

    placeholders = ", ".join(["%s"] * len(col_names))
    insert_sql = f"INSERT INTO {quote_ident(table)} ({', '.join(quote_ident(c) for c in col_names)}) VALUES ({placeholders})"

    total_rows = 0
    while True:
        rows = sql_cur.fetchmany(BATCH_SIZE)
        if not rows:
            break
        maria_cur.executemany(insert_sql, [tuple(row) for row in rows])
        total_rows += len(rows)
    maria_conn.commit()

    source_count = total_rows
    maria_cur.execute(f"SELECT COUNT(*) FROM {quote_ident(table)}")
    dest_count = maria_cur.fetchone()[0]
    return source_count, dest_count


def quote_ident_sqlserver(name: str) -> str:
    return f"[{name}]"


def main() -> int:
    if not APP_PASSWORD_FILE.exists():
        print(f"missing {APP_PASSWORD_FILE}", file=sys.stderr)
        return 1

    mismatches = []
    grand_total = 0

    for database in DATABASES:
        print(f"\n=== {database} ===")
        sql_conn = sql_server_connect(database)
        maria_conn = mariadb_connect(database)
        maria_conn.cursor().execute("SET FOREIGN_KEY_CHECKS=0")

        tables = get_tables(sql_conn)
        for table in tables:
            source_count, dest_count = migrate_table(sql_conn, maria_conn, table)
            grand_total += dest_count
            status = "OK" if source_count == dest_count else "MISMATCH"
            print(f"  {table:40s} {source_count:>8d} -> {dest_count:>8d}  {status}")
            if source_count != dest_count:
                mismatches.append(f"{database}.{table}: source={source_count} dest={dest_count}")

        maria_conn.cursor().execute("SET FOREIGN_KEY_CHECKS=1")
        maria_conn.commit()
        maria_conn.close()
        sql_conn.close()

    print(f"\nTotal rows migrated: {grand_total}")
    if mismatches:
        print("\nMISMATCHES:")
        for m in mismatches:
            print(f"  {m}")
        return 1

    print("All tables migrated with matching row counts.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
