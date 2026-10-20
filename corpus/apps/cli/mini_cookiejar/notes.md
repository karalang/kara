# mini_cookiejar

Python 3.13's `http.cookiejar` module, ported from `Lib/http/cookiejar.py`.
It is the cookie store behind `urllib.request`'s `HTTPCookieProcessor`,
and `requests` builds its own jar on top of it.

The port covers:
- the date parsers: `http2time`, with its strict HTTP format, the loose
  formats (rfc850, a missing weekday or clock, numeric and named
  timezones, the zone in parentheses) and the rule that puts a two-digit
  year within 50 years of the current one; `iso2time`; `time2isoz` and
  `time2netscape`;
- the header parsers: `split_header_words` (quoted values with
  backslash escapes, `,` between headers, junk skipped),
  `join_header_words`, which quotes a value that is not a token, and
  `parse_ns_headers`, the ad-hoc Netscape parser that lowercases the
  known attributes, parses `expires` and adds `version=0`;
- the domain rules: `domain_match`, `user_domain_match`, `is_HDN`,
  `reach`, `escape_path`, the request host with its `.local` form, the
  request path and port, and third-party detection;
- `Cookie`, with its `str()` and `repr()`;
- `DefaultCookiePolicy`, with every `set_ok_*` and `return_ok_*` check
  in order (version, verifiability, name, path, domain, port, secure,
  expiry), block and allow lists, `strict_domain`, the `strict_ns_*`
  options and `secure_protocols`, and the debug log line each check
  writes;
- `CookieJar`: `extract_cookies` (Set-Cookie and Set-Cookie2, Max-Age,
  cookies that arrive already expired and delete the stored one, RFC
  2109 cookies downgraded to Netscape ones), `add_cookie_header` (the
  longest path first, the RFC 2965 `$Version`, `$Path`, `$Domain` and
  `$Port` attributes, and `Cookie2`), the nested domain, path and name
  store, `clear` with its `KeyError`s, and the session and expiry sweeps;
- `MozillaCookieJar` (`cookies.txt`, with `#HttpOnly_` lines) and
  `LWPCookieJar` (Set-Cookie3 lines): `load`, `save` with
  `ignore_discard` and `ignore_expires`, and `revert`, which keeps the
  old cookies when the load fails.

`http.cookiejar` uses `re`, so this carries the regular expression
engine from mini_re and compiles the module's own patterns with it.
Requests are `urllib.request.Request` as the module sees one.

`mk.py` writes `data/`: `jobs.tsv` (seven jobs on a clock the jobs set;
the format is described at the top of `mk.py`), cookie files in both
formats, some of them broken, and the exchanges for two programs:
- a shop login whose cookies are saved to `cookies.txt` and loaded again
  by the next process, two hours later;
- a third-party cookie audit of the pages a news site and a shop pull
  in, with a block list and strict rules for unverifiable requests.

`oracle.py` runs the same jobs through the real module with its debug
log on, and it wrote `expected.out`.

The quirks are kept. Some examples:
- a date in the strict format comes back as a float (`760832612.0`) and
  every other one as an int;
- `Sun, 06 Jxx 1994 08:49:37 GMT` passes the strict pattern and then
  raises `ValueError: 'jxx' is not in list`;
- `31 Feb 2024` is 2 March, and `24:00:61` is accepted;
- a cookie loaded from `#HttpOnly_` is saved with the prefix again, but
  one set by an `HttpOnly` header is not, because the module looks for
  the attribute `HTTPOnly`;
- a bad line part-way through `cookies.txt` leaves the cookies read
  before it in the jar;
- a cookie for `Domain=localhost` is stored as `.localhost` and never
  sent back.

It matched on every backend the first time it ran; no gaps were found.
