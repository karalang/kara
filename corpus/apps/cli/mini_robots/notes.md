# mini_robots

Python 3.13's `urllib.robotparser`, the robots.txt reader most Python
crawlers use, ported from `robotparser.py` in its RFC 9309 form. Groups
are merged by user agent, rules use `*` and `$` wildcards, and the
longest match wins, with Allow winning ties. `Crawl-delay`,
`Request-rate` and `Sitemap` are read, and `read()` handles HTTP status
codes. The parts of `urllib.parse` it stands on are ported too:
`urlsplit` and `urlunsplit`, and `quote` and `unquote` with
`surrogateescape`. A polite crawler is built on the parser.

The run writes a small web of test sites under `data/web/<host>/`: each
host has a robots.txt, maybe an HTTP status for it, and `pages.json`.
Seven more robots.txt files go under `data/robots/`. Then the run does
the 58 jobs of `data/jobs.jsonl`, each printed after `== ` and the job:
- `load` reads a robots.txt through a fake `urlopen` that serves
  `data/web/`;
- `parse` parses a file from `data/robots/` the way `read()` would;
- `show` prints `str()` of the parser, `site_maps()` and the parser's
  flags, plus `crawl_delay()` and `request_rate()` for 18 user agents;
- `check` prints `can_fetch()` for each user agent over 91 URLs, as a
  row of `Y` (may fetch), `n` (may not) and `!` (raised);
- `crawl` walks the sites breadth first as one user agent. For each
  host it reads robots.txt first, then fetches only the pages it may.
  It follows links from the root and absolute links, and waits out each
  host's Crawl-delay on a virtual clock.

`oracle.py` runs the same jobs through the real `urllib.robotparser`
(CPython 3.13), with only `urlopen` replaced by the same fake web, and
wrote `expected.out`.

The library's quirks are kept. Some examples:
- A group does not inherit the `*` rules. KaraBot's own group has no
  `Disallow: /cart`, so the crawler fetches `/cart` as KaraBot while
  `*` agents may not.
- Groups for the same agent are merged by their lowercased name, but the
  merged group keeps only the agents that both spell the same way.
  shop.test has `KaraBot` in one group and `karabot` in a later one; the
  merged group has no agents, so `KaraBot/1.0` falls through to the
  first group again and `/drafts` stays open to it.
- Only the product token is matched: `Mozilla/5.0 (compatible;
  KaraBot/1.0)` is `mozilla`, so it gets the `*` group.
- An empty `User-agent:` line names an agent that is a substring of
  every product token, so that group takes every agent that no earlier
  group takes. A later `Bad Bot` group is reached only by the agent
  named exactly `Bad Bot`, and the `*` group only by the agent `*`,
  through the lookup of the whole name that comes first.
- A file that starts with a byte order mark has no rules: the first
  field reads `﻿user-agent`.
- `Disallow: /a%2fb` blocks `/a/b`, because `%2f` is decoded and `/` is
  not quoted again. Invalid UTF-8 in a rule stays as the same `%XX`
  bytes.
- `Crawl-delay: ٣` (Arabic-Indic three) and `Crawl-delay: ５` (full
  width) are read as 3 and 5. `1.5` and `-1` are ignored, and a
  30-digit delay is kept whole.
- A 401 or 403 for robots.txt disallows everything, and so do 5xx codes.
  Other 4xx codes, such as 404 and 410, allow everything. A host that
  does not resolve makes `read()` raise `URLError`; the parser then
  answers False to every `can_fetch()`, as it does before any read.
- `can_fetch(agent, "http://[::1/x")` raises `ValueError`. A rule with a
  `$` in the middle is dropped without a word.

Limits of the port:
- `urlsplit` does not check bracketed IPv6 netlocs or NFKC-normalize
  non-ASCII ones. No job uses either.
- Python keeps an undecodable byte as a lone surrogate. A Kāra String
  cannot hold one, so the port keeps the byte as the private-use code
  point U+10FF00 + byte, which no test file uses otherwise.
- `isdecimal()` and `int()` know twenty scripts' digits, not all of
  Unicode's.
- The fake web has no redirects, and it ignores the query on
  `/robots.txt`.

The first full run matched the oracle on every backend, with no
workarounds and no new rows.

Written 2026-10-19 by the real-world apps thread; karac at 7438e51233.
