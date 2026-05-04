# design_studies/http_api_call/http_api_call.py
#
# GET a JSON endpoint, parse the response, print rows.
# Python variant — sync requests, the most common shape for this task.
#
# Requires: pip install requests

import sys
from dataclasses import dataclass

import requests


@dataclass
class User:
    id: int
    name: str
    email: str


def main() -> None:
    url = "https://jsonplaceholder.typicode.com/users"
    try:
        resp = requests.get(url, timeout=10)
        resp.raise_for_status()
        users = [
            User(id=u["id"], name=u["name"], email=u["email"])
            for u in resp.json()
        ]
    except requests.RequestException as e:
        print(f"error: {e}", file=sys.stderr)
        sys.exit(1)

    for u in users:
        print(f"{u.id}\t{u.name}\t{u.email}")


if __name__ == "__main__":
    main()
