"""docs/library/log.md § The JSON format and § The text format, against a
model of the page's rules built on Python's `json` (whose compact output
quotes a string as serde_json, and so `Json.stringify`, does: the short
escapes `\\b \\f \\n \\r \\t \\" \\\\`, `\\u00XX` for the other controls, nothing
else escaped). Random records (messages and string fields with spaces, `"`,
`=`, `\\`, controls, DEL and non-ASCII; integers to the i64 limits; bools;
`Option`s; nested `Vec`s; an empty string) are logged at random levels
through `json_backend` and `text_backend`, on both streams, with a random
`init` level, and every line is held to the model: key order `level`,
`msg`, fields in call order, `task_id` (`null` outside a task); the text
line's padded level and its quoting rule, a message with a control
character quoted as a field value is (B-2026-11-27-14). Records below the level must not
appear.

    python3 conformance/log_format.py [--seed N] [--cases N] [--lanes interp,build,asan]
"""

import argparse
import json
import random

from harness import Report, kara_str, run

LEVELS = ["Debug", "Info", "Warn", "Error"]
CHARS = list("abcXYZ019 _-.:/") + [" ", '"', "=", "\\", "\n", "\t", "\r", "\x01", "\x1f", "\x7f", "é", "€", "😀", "'", "\x85"]


def rand_str(rng, chars=CHARS):
    return "".join(rng.choice(chars) for _ in range(rng.randrange(0, 8)))


def control(c):
    """A control character as log.md's text format counts one: C0, DEL, C1."""
    return ord(c) < 0x20 or 0x7F <= ord(c) < 0xA0


def rand_value(rng, depth=0):
    """(kara expression, Python value of its Json tree)."""
    k = rng.randrange(7 if depth < 2 else 4)
    if k == 0:
        s = rand_str(rng)
        return f"{kara_str(s)}", s
    if k == 1:
        n = rng.choice([0, 1, -1, 42, 2**31, -2**31, 2**63 - 1, -2**63, rng.randrange(-10**9, 10**9)])
        lit = "(-9223372036854775807 - 1)" if n == -2**63 else str(n)
        return f"id({lit})", n
    if k == 2:
        b = rng.random() < 0.5
        return ("true" if b else "false"), b
    if k == 3:
        s = rand_str(rng)
        return f"{kara_str(s)}.to_string()", s
    if k == 4:
        if rng.random() < 0.5:
            return "none_i64()", None
        n = rng.randrange(-100, 100)
        return f"Some({n} as i64)", n
    if k == 5:
        items = [rng.randrange(-5, 5) for _ in range(rng.randrange(0, 4))]
        return "{ let v: Vec[i64] = [" + ", ".join(str(i) for i in items) + "]; v }", items
    items = [rand_str(rng) for _ in range(rng.randrange(0, 3))]
    return ("{ let v: Vec[String] = [" + ", ".join(f"{kara_str(s)}.to_string()" for s in items) + "]; v }",
            items)


def compact(v):
    return json.dumps(v, ensure_ascii=False, separators=(",", ":"))


def text_value(v):
    if isinstance(v, str):
        if v and not any(c in v for c in ' "=') and not any(control(c) for c in v):
            return v
        return compact(v)
    return compact(v)


def json_line(level, msg, fields):
    rec = {"level": level.lower(), "msg": msg}
    for k, v in fields:
        rec[k] = v
    rec["task_id"] = None
    return compact(rec)


def text_line(level, msg, fields):
    shown = compact(msg) if any(control(c) for c in msg) else msg
    return f"{level.upper():5} {shown}" + "".join(f" {k}={text_value(v)}" for k, v in fields)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cases", type=int, default=150)
    ap.add_argument("--lanes", default="interp,build")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("log_format")

    # One program per (backend, stream, init level): the chosen stream is
    # compared line by line, and the other must stay empty.
    for backend in ("json", "text"):
        for stream in ("Stdout", "Stderr"):
            floor = rng.choice(LEVELS)
            calls, want = [], []
            for i in range(args.cases):
                level = rng.choice(LEVELS)
                msg = rand_str(rng)
                fields = []
                for j in range(rng.randrange(0, 4)):
                    expr, val = rand_value(rng)
                    fields.append((f"f{j}_{rng.choice(['a', 'k', 'x'])}", expr, val))
                args_ = "".join(f", {k}: {e}" for k, e, _ in fields)
                calls.append(f"    log.{level.lower()}({kara_str(msg)}{args_});")
                if LEVELS.index(level) >= LEVELS.index(floor):
                    plain = [(k, v) for k, _, v in fields]
                    want.append(json_line(level, msg, plain) if backend == "json" else text_line(level, msg, plain))
            src = ("import std.log;\n\nfn none_i64() -> Option[i64] {\n    None\n}\n\n"
                   "fn id(x: i64) -> i64 {\n    x\n}\n\nfn main() {\n"
                   f"    log.init(log.{backend}_backend(log.Stream.{stream}), level: log.Level.{floor});\n"
                   + "\n".join(calls) + "\n}\n")
            res = run(src, backends=tuple(args.lanes.split(",")))
            labels = [f"{backend}/{stream}/{floor} line {i}" for i in range(len(want))]
            # Lines end at "\n" only: `splitlines` would also split at U+0085
            # and U+2028, which JSON and the text format write as they are.
            lines = lambda t: t.split("\n")[:-1] if t.endswith("\n") else t.split("\n") if t else []
            if res["check"][0] != 0:
                rep.failed.append("check refused:\n" + res["check"][1] + res["check"][2])
                continue
            for lane in ("interp", "build", "asan"):
                for code, out, err in res.get(lane, []):
                    if code != 0:
                        rep.failed.append(f"[{lane}] exit {code}: {err[-1500:]}")
                    mine, other = (out, err) if stream == "Stdout" else (err, out)
                    if other:
                        rep.failed.append(f"[{lane}] {backend}/{stream} wrote the other stream: {other[:200]!r}")
                    rep.compare(lane, labels, lines(mine), want)
    rep.finish()


if __name__ == "__main__":
    main()
