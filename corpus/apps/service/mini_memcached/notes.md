# mini_memcached

memcached is the cache in front of a great many databases. Clients
speak its text protocol over TCP. The program is a memcached server
that answers that protocol as memcached 1.6 does, over a real socket
on 127.0.0.1:47747, with a small memory limit so that eviction shows.
`mini_memcached.py` is the mirror. It is the same server fed the same
bytes, and its output is `expected.out`.

`data/sessions.txt` holds ten client sessions. Each one is a
connection: the client writes its commands and shuts down its side,
then the server reads them, answers, and closes, and the client reads
the reply. The server's clock is virtual, and `@advance` moves it
between sessions. The transcript shows each command (`>`), its data
block, and each reply line (`<`). Long blocks are shortened.

**Storage.**
- `set`, `add`, `replace`, `append`, `prepend` and `cas` take a key,
  flags, exptime and a byte count, then that many bytes and `\r\n`.
- `noreply` silences the answer.
- `cas` compares the item's unique value, which every store changes.
- A block whose length doesn't end at `\r\n` is a
  `CLIENT_ERROR bad data chunk`, and its leftover bytes are read as
  the next command, as memcached does.
- A bad number or a key over 250 bytes is a `CLIENT_ERROR`. Its data
  line is then read as a command.
- An item larger than the cache is a `SERVER_ERROR`, and its data is
  skipped.

**Reading.** `get` and `gets` take many keys; `gat` and `gats` also
set a new exptime. `touch`, `delete` (with the legacy `0`), `incr` and
`decr` follow memcached: `incr` wraps at 2^64, `decr` stops at 0, and a
value that isn't a decimal is refused.

**Expiry.** An exptime of 0 never expires. Up to 30 days it counts
seconds from now. A larger one is an absolute Unix time, already
expired if it is before the server started. A negative one is already
expired. `flush_all` empties the cache at once. `flush_all N` makes
everything stored before N seconds from now invalid once that moment
passes.

**Memory.** An item costs 48 bytes plus its key and data, against a
2048-byte limit. A store that doesn't fit first frees dead items from
the least recently used end, then evicts live ones. Every hit and store
moves an item to the other end. `stats` reports the counters, bytes,
items, evictions and reclaims.

`mk.py` writes the sessions. `gen_kara.py` prints them as the Kāra
data functions in the program.

## Edits from mini_memcached.orig.kara to mini_memcached.kara

These were author errors:
- `NetError` has no `Display`, and `{e:?}` is not implemented yet, so
  the socket errors are fixed messages, as in mini_statsd.
- `Map` indexing takes the `String` key itself, not a `Str` view.
- `Some(0)` in one branch of an `if` whose other branch is an
  `Option[i128]` needs the binding's type written out.

These work around gaps:
- `U64_MAX` is spelled `(1 << 64) - 1`. Written as the literal
  `18446744073709551615`, it panics `karac check` (B-2026-10-18-2,
  new).
- `cache.link(new.clone())` is written in all four storage branches.
  Inside the session loop, check calls a method's `own` argument
  moved twice when it is moved in exclusive branches (B-2026-10-18-3,
  new).
- `joined.clone()` where append and prepend store the joined value
  (B-2026-10-15-20).
- `touch` and `gat` set the exptime through `contains_key` and
  `items[key]`, because `Map` has no `get_mut` (B-2026-10-12-3).
- `Cache.stat` binds the counter before returning it. Returned
  straight from `get(..).unwrap_or(0)`, the compiled backends return
  the counter's address (B-2026-10-18-4, new).

Reverted 2026-10-18, karac at 79a6c2eb9a: B-2026-10-18-4 is fixed, so
`Cache.stat` returns `self.stats.get(name).unwrap_or(0)` directly again.
That is the original line, and the app still matches on all four MIR
surfaces.
