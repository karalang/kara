"""docs/library/secret.md § Access, against Python's `==`: random pairs of
`Secret[String]` and `Secret[Vec[u8]]` (equal, differing in one byte at
the start, middle or end, one a prefix of the other, empty, multibyte
text) through `ct_eq` both ways, and after an in-place change through
`expose_mut` (a `push_str` or `push`, then a `clear`), each result and the
exposed value held to the model. `ct_eq`'s timing is conformance/ct_timing.py.

    python3 conformance/secret_model.py [--seed N] [--cases N] [--lanes interp,build,asan]
"""

import argparse
import random

from harness import Report, kara_str, run

ALPHA = list("ab09 _-") + ["é", "€", "😀", "\\", '"']


def rand_pair(rng, kind):
    base = (lambda n: "".join(rng.choice(ALPHA) for _ in range(n))) if kind == "s" else \
        (lambda n: [rng.randrange(256) for _ in range(n)])
    a = base(rng.randrange(0, 12))
    k = rng.randrange(6)
    if k == 0:
        b = a[:] if kind == "v" else a
    elif k == 1 and a:
        i = rng.choice([0, len(a) // 2, len(a) - 1])
        if kind == "s":
            b = a[:i] + ("x" if a[i] != "x" else "y") + a[i + 1:]
        else:
            b = a[:]
            b[i] ^= 1 << rng.randrange(8)
    elif k == 2 and a:
        b = a[:rng.randrange(len(a))]
    elif k == 3:
        b = a + (base(1) or ("z" if kind == "s" else [7]))
    else:
        b = base(rng.randrange(0, 12))
    return a, b


def lit(kind, v):
    if kind == "s":
        return f"{kara_str(v)}.to_string()"
    return "{ let v: Vec[u8] = [" + ", ".join(f"{x}u8" for x in v) + "]; v }"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cases", type=int, default=200)
    ap.add_argument("--lanes", default="interp,build")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("secret_model")
    body, want = [], []
    for c in range(args.cases):
        kind = rng.choice("sv")
        a, b = rand_pair(rng, kind)
        ty = "String" if kind == "s" else "Vec[u8]"
        tb = "true" if a == b else "false"
        body.append("    {\n"
                    f"        let mut x: Secret[{ty}] = Secret.new({lit(kind, a)});\n"
                    f"        let y: Secret[{ty}] = Secret.new({lit(kind, b)});\n"
                    f'        println(f"{c} {{x.ct_eq(y)}} {{y.ct_eq(x)}} {{x.ct_eq(x)}}");')
        want.append(f"{c} {tb} {tb} true")
        # An in-place change through expose_mut, then compare again.
        if kind == "s":
            extra = "".join(rng.choice(ALPHA) for _ in range(rng.randrange(0, 3)))
            body.append(f"        x.expose_mut().push_str({kara_str(extra)});")
            a2 = a + extra
            show = f'{{x.expose().len()}}'
            shown = str(len(a2.encode()))
        else:
            extra = [rng.randrange(256) for _ in range(rng.randrange(0, 3))]
            body += [f"        x.expose_mut().push({e}u8);" for e in extra]
            a2 = a + extra
            show = f'{{x.expose().len()}}'
            shown = str(len(a2))
        t2 = "true" if a2 == b else "false"
        body.append(f'        println(f"{c} after {{x.ct_eq(y)}} {show}");')
        want.append(f"{c} after {t2} {shown}")
        body.append("        x.expose_mut().clear();")
        te = "true" if len(b) == 0 else "false"
        body.append(f'        println(f"{c} cleared {{x.ct_eq(y)}} {{x.expose().len()}}");\n    }}')
        want.append(f"{c} cleared {te} 0")
    src = "import std.secret.{Secret};\n\nfn main() {\n" + "\n".join(body) + "\n}\n"
    res = run(src, backends=tuple(args.lanes.split(",")))
    rep.check_runs(res, [f"line {i}" for i in range(len(want))], want)
    rep.finish()


if __name__ == "__main__":
    main()
