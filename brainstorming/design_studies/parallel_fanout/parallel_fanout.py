# design_studies/parallel_fanout/parallel_fanout.py
#
# Fetch N user records concurrently and print aggregated output.
# Python variant — asyncio + httpx.
#
# Requires: pip install httpx

import asyncio
import sys
from dataclasses import dataclass

import httpx


@dataclass
class User:
    id: int
    name: str
    email: str


async def fetch(client: httpx.AsyncClient, user_id: int) -> User:
    resp = await client.get(
        f"https://jsonplaceholder.typicode.com/users/{user_id}"
    )
    resp.raise_for_status()
    u = resp.json()
    return User(id=u["id"], name=u["name"], email=u["email"])


async def main() -> None:
    ids = [1, 2, 3, 4, 5]
    async with httpx.AsyncClient() as client:
        try:
            users = await asyncio.gather(*(fetch(client, i) for i in ids))
        except httpx.HTTPError as e:
            print(f"error: {e}", file=sys.stderr)
            sys.exit(1)

    for u in users:
        print(f"{u.id}\t{u.name}\t{u.email}")


if __name__ == "__main__":
    asyncio.run(main())
