# service/config_svc

A configuration service of the kind that sits behind a fleet of workers.
Configuration comes in three layers: built-in defaults, an operator's
`config.json`, and environment overrides (`CFG_SERVER__PORT=9090` sets
`/server/port`, one variable for every scalar the schema describes). The
layers are combined with JSON Merge Patch (RFC 7386) and checked against a
schema: types, int ranges, string choices, array sizes and unknown keys. The
file layer is checked on its own too, so a bad value is refused even while
an environment override hides it.

Over loopback HTTP it serves the effective document and any value in it by
JSON Pointer (RFC 6901, `~0`/`~1` escapes, strict array indices), with an
ETag (FNV-1a of the canonical JSON) so readers can poll with `If-None-Match`
and get 304. Writes are a merge patch on `/config`
(`application/merge-patch+json`, else 415) or PUT/DELETE on a pointer. Every
write needs `If-Match` (428 without it, 412 on a stale tag, `*` for any),
is validated (422 listing every violation), is written back to
`config.json`, and carries a `Warning` header when the environment shadows
the value just changed. `POST /config/reload` re-reads the file after an
operator edits it and keeps the old config when the file is broken (400) or
invalid (422). The JSON reader and writer are hand-written, integers only.

The environment the run expects is in `app.toml`:
`CFG_SERVER__PORT=9090 CFG_FEATURES__BETA=true CFG_LIMITS__MAX_CONN=lots
CFG_LOG__LEVEL=debug`. `lots` shows a refused override.

The mirror `config_svc.py` is the same algorithm in Python (no `json`
module) and wrote `expected.out` under that environment.

Written 2026-10-15 by the real-world apps thread; karac at 0e5d2694d3.

## Edits from config_svc.orig.kara to config_svc.kara

Author errors: none reached the compiler. One design bug of mine showed in
the first run: validating only the layered result let `{"server": {"port":
70000}}` through because the environment's 9090 hid it. Both files validate
the file layer on its own now.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-14-53 (widened by this app): `karac check` reports a `String`
  passed to `env.var(name)` as moved, though the library declares
  `name: ref String`, the same defect as `fs.write`. Every MIR surface runs
  the original. Worked around with `env.var(var.clone())`.

Went in clean: a recursive `#[derive(Clone, PartialEq)]` enum, `mut ref`
recursion into `Vec` elements and tuple fields (`set_at(mut fields[j].1,
..)`), `Option[Resp]` early returns, `map_err(..)?` chains in `start`.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
