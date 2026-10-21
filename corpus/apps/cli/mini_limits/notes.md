# mini_limits

Rate limiting as limits 5.8.0 does it. limits is the library under
Flask-Limiter and slowapi, and many Python API gateways use it
directly. The port covers:
- rate limit items:
  - the six granularities, multiples and namespaces;
  - the keys a limit is counted under;
  - how items compare and sort;
- the rate limit string notation, such as `10/minute` and
  `5 per 2 hours; 1/second`. It is matched with limits' own regular
  expressions, through the regular expression engine of `mini_re`;
- the memory storage:
  - counters with an expiry time;
  - moving window entries;
  - sliding window counters;
  - the sweep its background timer does;
- the three strategies, fixed window, moving window and sliding window
  counter, with `hit`, `test`, `get_window_stats`, `clear` and costs;
- storage URIs, parsed with the `urllib.parse` code of `mini_urlparse`,
  and `storage_from_string` with the error each storage gives when its
  driver is not installed;
- four programs:
  - an API gateway with per-route limits, exemptions and per-client
    keys;
  - a login guard that locks out a user or an address;
  - tenant export quotas that charge by rows;
  - the three strategies side by side at a window boundary.

The run writes `data/` and then works through the 11 jobs in
`data/jobs.jsonl`, 561 operations in all:
- 69 limit strings and 2 byte strings, valid and broken;
- 12 items built directly, with keys, comparisons, sorting and
  granularity names;
- 70 memory storage calls on a clock the job moves;
- the same script of hits, tests, stats and clears under each strategy;
- 30 storage URIs and 28 calls to `storage_from_string`;
- the gateway over 7 routes and 268 requests, 37 login attempts and 40
  export requests.

The jobs set the clock. `oracle.py` runs the same jobs through limits
itself and wrote `expected.out`. It uses the Python files of the 5.8.0
wheel in `limits/`, unchanged. The oracle replaces `time.time` with the
jobs' clock and the memory storage's timer with one that never fires,
so expired entries stay until a job sweeps them. It also stands in for
the `deprecated` package, whose decorators only add version notes to
docstrings.

The quirks are kept. Some examples:
- The pattern for several limits has `(:?` where `(?:` was meant, so a
  stray colon is allowed before a separator: `1/second:,2/day` parses.
- A multiple of 0 means 1, so `1 per 0 minute` is `1 per 1 minute`.
- Items compare by granularity alone. `10 per 1 minute` is not equal to
  `5 per 1 minute` and not less than it, but it is greater. Equality
  ignores the namespace, although the namespace changes the key.
- Under IGNORECASE, `[a-z]` matches `ſ`, the long s, because sre treats
  it as a form of `s`. So `1/ſecond` passes the pattern and then fails
  as "no granularity matched for ſecond". The engine copied from
  `mini_re` did not know these pairs; this copy adds sre's table of
  them.
- A fixed window starts at its first hit, while a sliding window
  counter's windows start at multiples of the period since the epoch.
- A moving window whose entries have all expired, but have not been
  swept, reports its reset time from the oldest stale entry, which is
  in the past.
- A sweep empties a moving window's list but keeps the key, so `reset`
  still counts it.
- `decr` on an unknown key stores a counter with no expiry, and the
  next `get` removes it.
- A cost larger than the limit is refused before anything is counted.
- `Valkey://` gives the redis error, because the check for valkey looks
  at the URI as written and only the scheme is lowered.
- `redis+sentinel://s1:26379/` gets past the check for a service name,
  since the name is the path without slashes, an empty string.

The errors for redis, valkey and pymemcache are what limits says when
those packages are not installed, as on the oracle's run.

Not ported:
- the redis, valkey, memcached and MongoDB storages themselves, and
  the asynchronous API;
- item hashes, which Python randomizes per run.

## License

limits is under the MIT license in `LICENSE-limits.txt`:

    Copyright (c) 2023 Ali-Akber Saifee

That covers the files in `limits/`.
