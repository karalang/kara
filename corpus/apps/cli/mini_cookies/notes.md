# mini_cookies

HTTP cookies as Python 3.13's `http.cookies` handles them, with a
browser's cookie jar built on top. Every web framework parses `Cookie`
headers and writes `Set-Cookie` ones, and every HTTP client keeps a jar.
The program ports `SimpleCookie` with its quoting rules and its exact
error messages, and adds the RFC 6265 rules a client applies before it
stores or sends a cookie.

The run has three parts:
- **load**: the 26 `Cookie` headers in `data/headers.txt`. For each one
  the program prints the parsed cookie, every morsel with its value,
  coded value and attributes, and the `Set-Cookie` lines it would write
  back. `load` follows `_CookiePattern`, ported as a backtracking
  matcher: its lazy key, quoted values with backslash escapes, the
  `Expires` date form that may hold a comma, and a value that ends at a
  space, a `;` or the end of the header. The headers cover:
  - octal escapes in quoted values;
  - `$` attributes;
  - reserved attributes, and `Secure`/`HttpOnly` as flags;
  - an illegal key;
  - a key with no value, and a value with no key;
  - duplicate keys;
  - a header that stops parsing half way, so nothing is set.
- **build**: sets cookies by hand, with an integer `Max-Age`, flags, a
  value that needs quoting and a latin-1 value written in octal. Seven
  bad assignments print their `CookieError`: an illegal key, a reserved
  key, an unknown attribute and control characters. It then prints the
  full output, the output limited to `path` and `httponly` under a
  `Cookie:` header, a morsel's `repr` and `js_output`.
- **jar**: replays the 16 `Set-Cookie` headers and 10 requests in
  `data/jar.txt` against a clock. The jar applies:
  - domain matching, and the rejection of a domain that is too broad or
    does not match the host;
  - host-only cookies and default paths;
  - `Secure` set only from an https origin, and sent only over https;
  - `Max-Age` before `Expires`, with a bad value of either ignored;
  - replacement that keeps the creation time, and deletion by an
    expiry in the past;
  - the request order: longer paths first, then older cookies.

  It dumps the jar after each round of responses.

Not supported: the jar's public-suffix list (it rejects only a domain
with no dot), `SameSite` enforcement, `Partitioned`, and the jar's size
limits. The date parser takes only the RFC 1123 form and its
dash-separated variant, not RFC 6265's lenient algorithm. The jar is the
program's own and is the same in both versions.

**Oracle.** `expected.out` is the output of `oracle.py`, the same
program written with `http.cookies`. Run it from this directory after
`mk.py` writes `data/`.

## Edits from mini_cookies.orig.kara to mini_cookies.kara

Compiler gaps:
- **B-2026-10-06-86:** `load` collected its items in
  `enum Item { Attribute(String, Attr), KeyValue(String, String) }`.
  `karac check` passed it, but every running surface refused it with
  `E_ENUM_NESTED_ENUM_PAYLOAD`, because `Attr` is an enum. `Item` is now
  a struct with an `is_attr` flag. The row already records that check
  accepts this shape.
- **B-2026-10-15-32:** `output_string` matched `v` with an `Attr.Text(t)`
  arm and used `v` again in the `_` arm. check treated `v` as moved in
  every arm. It now matches `v.clone()`.

Author errors:
- `Item` needed `#[derive(Clone)]`.

After that, `karac check` passed. The output matched `expected.out` on
the first run under `__mir-run`, `__mir-llvm-run` and `__mir-llvm-asan`.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
