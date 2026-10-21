# mini_uaparser

ua-parser 0.18.0, the Python user agent parser of the uap-core project
(regexes and code by Google and the ua-parser authors), the library
analytics jobs, ad servers and log pipelines use to turn a `User-Agent`
header into a browser, an operating system and a device. Its parsers are
the 350 user agent, 197 operating system and 628 device regexes of
uap-core's `regexes.yaml`, in the order `_regexes.py` lists them. The
port covers:
- `Parse`, `ParseUserAgent`, `ParseOS` and `ParseDevice`: the first
  parser whose name comes out truthy wins, replacements with `$1` (user
  agents) or any `$N` (`MultiReplace`, with its stripping and its `''`
  as `None`), and `Other` when nothing matches;
- the parse cache, keyed by the string and the sorted overrides,
  cleared when it holds `MAX_CACHE_SIZE` (200) entries;
- the deprecated javascript overrides (`js_user_agent_family`, the
  versions and the Chrome Frame rewrite from `js_user_agent_string`),
  `ParseWithJSOverrides`, `GetFilters` and the `DeprecationWarning`s
  they raise;
- `PrettyUserAgent`, `PrettyOS` and `Pretty`, and `MatchSpans` of one
  parser.

`mk.py` writes `data/`:
- `access.log`: 165 lines of a web shop's front end in the combined log
  format (five of them a TLS handshake sent to the plain port), from
  documentation addresses;
- `agents.txt`: the 22 user agents in it: desktop and mobile browsers,
  crawlers, command line tools, an old Chrome Frame, `-` and nothing;
- `jobs.tsv`: 122 jobs. They parse each agent, report on the log
  (requests by browser, operating system, device and brand, the bots'
  traffic and the errors by browser), call the overrides, the pretty
  printers and `MatchSpans`, and fill the cache past its size.

Each job prints `-- ` and its columns, then what it returns with
`repr`. A Python exception prints `! `, its class and its message, and
the warnings a job raised follow it, counted.

`oracle.py` runs the jobs through ua_parser itself (the copy in
`ua_parser/`, unchanged) and wrote `expected.out`. `gen_kara.py` writes
the generated section at the end of `mini_uaparser.kara`: every parser
with its pattern, the flags `re.compile` gave it and its replacements,
and `data/`.

ua_parser finds everything with `re`, so this carries the regex engine
of `mini_re` (Python 3.13's parser and an sre-style backtracking
matcher) and compiles all 1175 patterns with it. Like sre's INFO block,
the engine skips work it can prove useless: a pattern is not tried on a
string that lacks the text every match of it holds (the longest run of
literals, or one run for each branch of an alternation every match goes
through), and a search does not start at a character no match can start
with. A `$1` family replacement takes the group's text as it is (sre
would read backslashes in it; no agent here has one).

The quirks are kept. Some examples:
- when nothing matches, the versions come from the last parser tried,
  and an operating system's versions are not `or None`'d, so one can be
  `''`;
- a result's dict lists its parts in the order they were first asked
  for, so `ParseOS` before `Parse` puts `os` before `user_agent`;
- a Chrome Frame override parses the javascript agent through the
  cache, and when that clears the cache, the outer result goes into a
  dict the cache no longer holds, so the next call parses it again;
- `MatchSpans` of a pattern without groups adds 1 to `lastindex`, which
  is `None`, and raises a `TypeError`.

ua-parser and uap-core's regexes are under the Apache License 2.0, in
`LICENSE-ua-parser.txt`.
