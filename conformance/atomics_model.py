"""docs/library/concurrency.md § Atomics, against a model of the page's
rules: random scripts of `load`, `store`, `swap`, `compare_exchange`,
`fetch_add`, `fetch_sub`, `fetch_and`, `fetch_or` and `fetch_xor` on one
`Atomic[T]` for each `T` the page lists (`bool`, `i32`, `i64`, `u32`,
`u64`), with operands at and around each type's limits. Every operation's
result and the value it leaves are printed and held to the model: each
`fetch_*` returns the previous value, `fetch_add` and `fetch_sub` wrap and
never panic, `compare_exchange` stores only on equality and returns
`Ok(previous)` or `Err(actual)`, and the bitwise ones are logical on
`bool`. One task, so every result is determined.

    python3 conformance/atomics_model.py [--seed N] [--ops N] [--lanes interp,build,asan]
"""

import argparse
import random

from harness import Report, run

TYPES = {
    "i32": (-2**31, 2**31 - 1),
    "i64": (-2**63, 2**63 - 1),
    "u32": (0, 2**32 - 1),
    "u64": (0, 2**64 - 1),
}
INT_OPS = ["load", "store", "swap", "cx", "cx_hit", "add", "sub", "and", "or", "xor"]
BOOL_OPS = ["load", "store", "swap", "cx", "cx_hit", "and", "or", "xor"]


def wrap(t, n):
    lo, hi = TYPES[t]
    m = hi - lo + 1
    return (n - lo) % m + lo


def lit(t, n):
    """A Kara expression of type `t` with value `n`, by way of `id_<t>` so
    the literal's type is the parameter's."""
    if t == "bool":
        return "true" if n else "false"
    lo, _ = TYPES[t]
    if n == lo and lo < 0:
        return f"(id_{t}({n + 1}) - 1)"
    return f"id_{t}({n})"


def rand_val(rng, t):
    if t == "bool":
        return rng.random() < 0.5
    lo, hi = TYPES[t]
    return rng.choice([lo, lo + 1, hi, hi - 1, 0, 1, -1 if lo < 0 else 2, 12, 10, 3, 0x5555_5555,
                       rng.randrange(lo, hi + 1)])


def show(t, v):
    return ("true" if v else "false") if t == "bool" else str(v)


def script(rng, t, n_ops):
    """(kara statements, expected lines)."""
    cur = rand_val(rng, t)
    stmts = [f"    let a = Atomic[{t}].new({lit(t, cur)});"]
    want = []
    for i in range(n_ops):
        op = rng.choice(BOOL_OPS if t == "bool" else INT_OPS)
        v = rand_val(rng, t)
        if op == "load":
            stmts.append(f'    println(f"load {{a.load()}}");')
            want.append(f"load {show(t, cur)}")
            continue
        if op == "store":
            stmts.append(f"    a.store({lit(t, v)});")
            cur = v
        elif op in ("cx", "cx_hit"):
            expect = cur if op == "cx_hit" else v
            new = rand_val(rng, t)
            stmts.append(f"    match a.compare_exchange({lit(t, expect)}, {lit(t, new)}) {{\n"
                         f'        Ok(p) => println(f"cx ok {{p}}"),\n'
                         f'        Err(p) => println(f"cx err {{p}}"),\n'
                         "    }")
            if expect == cur:
                want.append(f"cx ok {show(t, cur)}")
                cur = new
            else:
                want.append(f"cx err {show(t, cur)}")
        else:
            meth = {"swap": "swap", "add": "fetch_add", "sub": "fetch_sub", "and": "fetch_and",
                    "or": "fetch_or", "xor": "fetch_xor"}[op]
            stmts.append(f'    println(f"{op} {{a.{meth}({lit(t, v)})}}");')
            want.append(f"{op} {show(t, cur)}")
            if t == "bool":
                cur = {"swap": v, "and": cur and v, "or": cur or v, "xor": cur != v}[op]
            else:
                lo, _ = TYPES[t]
                bits = 32 if t in ("i32", "u32") else 64
                mask = 2**bits - 1
                if op == "swap":
                    cur = v
                elif op == "add":
                    cur = wrap(t, cur + v)
                elif op == "sub":
                    cur = wrap(t, cur - v)
                else:
                    a, b = cur & mask, v & mask
                    cur = wrap(t, {"and": a & b, "or": a | b, "xor": a ^ b}[op])
        stmts.append(f'    println(f"now {{a.load()}}");')
        want.append(f"now {show(t, cur)}")
    return stmts, want


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--ops", type=int, default=120)
    ap.add_argument("--lanes", default="interp,build")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("atomics_model")
    helpers = "".join(f"fn id_{t}(x: {t}) -> {t} {{\n    x\n}}\n\n" for t in TYPES)
    body, want = [], []
    for t in ("bool", *TYPES):
        for k in range(3):
            stmts, w = script(rng, t, args.ops // 3)
            body.append("    {\n" + "\n".join("    " + s for s in stmts) + "\n    }")
            want += w
    src = "import std.sync.{Atomic};\n\n" + helpers + "fn main() {\n" + "\n".join(body) + "\n}\n"
    res = run(src, backends=tuple(args.lanes.split(",")))
    labels = [f"line {i}: {w}" for i, w in enumerate(want)]
    rep.check_runs(res, labels, want)
    rep.finish()


if __name__ == "__main__":
    main()
