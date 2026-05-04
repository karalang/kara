# design_studies/db_read/postgres_query.py
#
# Connect to Postgres and print rows from a `users` table.
# Companion to the Java, Rust (minimal/production), and
# Kāra (direct/injected) variants in this directory.
#
# Requires: pip install psycopg[binary]

import os
import sys

import psycopg


def main() -> None:
    url = os.environ.get("DATABASE_URL")
    if url is None:
        print("DATABASE_URL not set", file=sys.stderr)
        sys.exit(1)

    try:
        with psycopg.connect(url) as conn, conn.cursor() as cur:
            cur.execute("SELECT id, name, email FROM users ORDER BY id")
            for id_, name, email in cur:
                print(f"{id_}\t{name}\t{email}")
    except psycopg.Error as e:
        print(f"DB error: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
