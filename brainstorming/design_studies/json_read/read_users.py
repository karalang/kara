# design_studies/json_read/read_users.py
#
# Read a JSON array of users from disk and print rows.
# Usage: python read_users.py <path>

import json
import sys
from dataclasses import dataclass


@dataclass
class User:
    id: int
    name: str
    email: str


def main() -> None:
    if len(sys.argv) < 2:
        print("usage: read_users.py <path>", file=sys.stderr)
        sys.exit(1)

    path = sys.argv[1]
    try:
        with open(path, encoding="utf-8") as f:
            raw = json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        print(f"error: {e}", file=sys.stderr)
        sys.exit(1)

    users = [User(**u) for u in raw]
    for u in users:
        print(f"{u.id}\t{u.name}\t{u.email}")


if __name__ == "__main__":
    main()
