"""docs/library/pool.md, against a model of the page's rules. Random scripts
in one task over two pools of a `Drop` type `Conn` whose drop prints its
id: `acquire` (a 1 ms timeout), `release` into the loan's own pool or the
other one, dropping a loan, `close`, and switching a pool's factory between
succeeding and failing; some pools have a health check that fails every id
divisible by 3. At the end the pools drop, then the loans still out.

The page does not say which idle value `acquire` reuses, so the check reads
the program's choice from its output and holds it to the rules: an idle
value that passes the check is reused before a new one is made; only values
that fail the check are dropped on the way; below `max_connections` a new
value is made, a factory error is `CreateFailed` and counts toward nothing;
at `max_connections` a caller outside any task waits the timeout and gets
`Timeout`; a value goes back once per loan, `release` included; a loan
released into the other pool is dropped and stops counting toward its own;
`close` drops the idle values at once and each later return, and every later
`acquire` is `PoolClosed`; a dropped pool drops its idle values, and a loan
that outlives it drops its value when it goes.

    python3 conformance/pool_model.py [--seed N] [--scripts N] [--steps N] [--lanes interp,build,asan]
"""

import argparse
import random

from harness import Report, run

HEAD = """import std.pool.{Pool, PoolError, PooledConnection};
import std.sync.{Atomic};
import std.time.{Duration};

struct Conn { id: i64 }

impl Drop for Conn {
    fn drop(mut ref self) {
        println(f"drop {self.id}");
    }
}

struct ConnectError { n: i64 }

shared struct Ctl { fail: Atomic[bool], n: Atomic[i64] }

fn make(c: Ctl, base: i64) -> Result[Conn, ConnectError] {
    let n = base + c.n.fetch_add(1) + 1;
    if c.fail.load() { Err(ConnectError { n: n }) } else { Ok(Conn { id: n }) }
}

"""

NAMES = ("a", "b")


class P:
    def __init__(self, rng, base):
        self.base, self.max = base, rng.choice([1, 2, 3])
        self.health = rng.random() < 0.5
        self.idle, self.out, self.calls = set(), 0, 0
        self.idle_pass = self.idle_fail = 0
        self.fail, self.closed, self.alive = False, False, True

    def passes(self, i):
        return not self.health or i % 3 != 0


def gen(rng, n_steps, tag):
    """The script's kara body and its ops, each (kind, args), in order."""
    pools = [P(rng, 1000), P(rng, 2000)]
    lines = []
    for n, p in zip(NAMES, pools):
        hc = ".with_health_check(|c| c.id % 3 != 0)" if p.health else ""
        lines += [f"    let c{n} = Ctl {{ fail: Atomic.new(false), n: Atomic.new(0) }};",
                  f"    let c{n}2 = c{n}.clone();",
                  f"    let p{n}: Pool[Conn, ConnectError] = Pool.new(|| make(c{n}2, {p.base}), {p.max}, 4){hc};"]
    lines.append("    let mut loans: Vec[PooledConnection[Conn]] = Vec.new();")
    ops = []
    loans = []  # (pool index, whether its value passes the check)
    for k in range(n_steps):
        r = rng.random()
        m = f"{tag}{k}"
        if r < 0.45:
            j = rng.randrange(2)
            n = NAMES[j]
            lines.append(f"    match p{n}.acquire(Duration.ms(1)) {{\n"
                         "        Ok(c) => {\n"
                         f'            println(f"{m} ok {{c.val.id}}");\n'
                         "            loans.push(c);\n"
                         "        }\n"
                         f'        Err(PoolError.Timeout) => println("{m} timeout"),\n'
                         f'        Err(PoolError.Busy) => println("{m} busy"),\n'
                         f'        Err(PoolError.CreateFailed(e)) => println(f"{m} failed {{e.n}}"),\n'
                         f'        Err(PoolError.PoolClosed) => println("{m} closed"),\n'
                         f'        Err(_) => println("{m} other"),\n'
                         "    }")
            ops.append(("acq", j))
            p = pools[j]
            # Mirror the outcome by counts: whether an acquire succeeds, and
            # whether the value it lends passes the check, do not depend on
            # which idle value it picks.
            if p.closed:
                continue
            if p.idle_pass:
                p.idle_pass -= 1
                p.out += 1
                loans.append((j, True))
            else:
                p.idle_fail = 0
                if p.out < p.max:
                    p.calls += 1
                    if not p.fail:
                        p.out += 1
                        loans.append((j, p.passes(p.base + p.calls)))
            continue
        if r < 0.65 and loans:
            i = rng.randrange(len(loans))
            j, ok = loans.pop(i)
            kind = rng.choice(["rel", "rel", "drop", "other"])
            if kind == "drop":
                lines.append(f"    drop(loans.remove({i}));")
            else:
                n = NAMES[j] if kind == "rel" else NAMES[1 - j]
                lines.append(f"    p{n}.release(loans.remove({i}));")
            lines.append(f'    println("{m} {kind}");')
            ops.append((kind, i))
            p = pools[j]
            p.out -= 1
            if kind != "other" and not p.closed:
                if ok:
                    p.idle_pass += 1
                else:
                    p.idle_fail += 1
            continue
        if r < 0.72:
            j = rng.randrange(2)
            lines.append(f"    p{NAMES[j]}.close();")
            lines.append(f'    println("{m} close");')
            ops.append(("close", j))
            pools[j].closed = True
            pools[j].idle_pass = pools[j].idle_fail = 0
            continue
        j = rng.randrange(2)
        v = not pools[j].fail
        lines.append(f"    c{NAMES[j]}.fail.store({'true' if v else 'false'});")
        lines.append(f'    println("{m} fail {v}");')
        ops.append(("fail", j))
        pools[j].fail = v
    lines += ["    drop(pa);", f'    println("{tag}end a");', "    drop(pb);", f'    println("{tag}end b");',
              "    drop(loans);", f'    println("{tag}end");']
    return "\n".join(lines), ops


def check(lines, ops, rng_pools, tag):
    """Replays `lines` (the script's output) against the rules; returns the
    number of lines it used, or raises AssertionError with the reason."""
    pools = rng_pools
    loans = []  # (pool index, id)
    pos = [0]

    def until(marker_prefix):
        drops = []
        while True:
            if pos[0] >= len(lines):
                raise AssertionError(f"output ended before {marker_prefix!r}")
            line = lines[pos[0]]
            pos[0] += 1
            if line.startswith("drop "):
                drops.append(int(line[5:]))
                continue
            if not line.startswith(marker_prefix):
                raise AssertionError(f"got {line!r}, want a line starting {marker_prefix!r}")
            return drops, line[len(marker_prefix):]

    def expect_drops(got, want, what):
        if sorted(got) != sorted(want):
            raise AssertionError(f"{what}: dropped {sorted(got)}, want {sorted(want)}")

    for k, (kind, a) in enumerate(ops):
        m = f"{tag}{k} "
        drops, rest = until(m)
        if kind == "acq":
            p = pools[a]
            if p.closed:
                expect_drops(drops, [], f"{m}acquire on a closed pool")
                if rest != "closed":
                    raise AssertionError(f"{m}{rest}: want closed")
                continue
            passing = {i for i in p.idle if p.passes(i)}
            failing = p.idle - passing
            if passing:
                if not rest.startswith("ok "):
                    raise AssertionError(f"{m}{rest}: want ok, an idle value passes ({sorted(passing)})")
                x = int(rest[3:])
                if x not in passing:
                    raise AssertionError(f"{m}ok {x}: not one of the idle values that pass {sorted(passing)}")
                if not set(drops) <= failing or len(set(drops)) != len(drops):
                    raise AssertionError(f"{m}dropped {drops} reusing {x}; only failing idle {sorted(failing)} may drop")
                p.idle -= set(drops) | {x}
                p.out += 1
                loans.append((a, x))
                continue
            expect_drops(drops, failing, f"{m}acquire with no idle value that passes")
            p.idle = set()
            if p.out >= p.max:
                if rest != "timeout":
                    raise AssertionError(f"{m}{rest}: want timeout ({p.out} out of max {p.max})")
                continue
            p.calls += 1
            n = p.base + p.calls
            if p.fail:
                if rest != f"failed {n}":
                    raise AssertionError(f"{m}{rest}: want failed {n}")
                continue
            if rest != f"ok {n}":
                raise AssertionError(f"{m}{rest}: want ok {n} (a new value)")
            p.out += 1
            loans.append((a, n))
        elif kind in ("rel", "drop", "other"):
            j, x = loans.pop(a)
            p = pools[j]
            p.out -= 1
            if rest != kind:
                raise AssertionError(f"{m}{rest}: want {kind}")
            if kind == "other" or p.closed or not p.alive:
                expect_drops(drops, [x], f"{m}{kind} of {x}")
            else:
                expect_drops(drops, [], f"{m}{kind} of {x}")
                p.idle.add(x)
        elif kind == "close":
            p = pools[a]
            expect_drops(drops, p.idle, f"{m}close")
            p.idle = set()
            p.closed = True
            if rest != "close":
                raise AssertionError(f"{m}{rest}: want close")
        else:
            p = pools[a]
            expect_drops(drops, [], f"{m}fail")
            p.fail = not p.fail
            if rest != f"fail {p.fail}":
                raise AssertionError(f"{m}{rest}: want fail {p.fail}")
    for j, n in enumerate(NAMES):
        drops, rest = until(f"{tag}end {n}")
        expect_drops(drops, pools[j].idle, f"dropping pool {n}")
        pools[j].idle = set()
        pools[j].alive = False
    drops, rest = until(f"{tag}end")
    expect_drops(drops, [x for _, x in loans], "dropping the loans that outlived their pools")
    return pos[0]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--scripts", type=int, default=30)
    ap.add_argument("--steps", type=int, default=30)
    ap.add_argument("--lanes", default="interp,build")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("pool_model")
    cases, fns = [], []
    for s in range(args.scripts):
        state = rng.getstate()
        body, ops = gen(rng, args.steps, f"c{s}.")
        cases.append((s, state, ops))
        fns.append(f"fn case{s}() {{\n{body}\n}}\n")
    src = HEAD + "\n".join(fns) + "\nfn main() {\n" + "\n".join(f"    case{s}();" for s in range(args.scripts)) + "\n}\n"
    res = run(src, backends=tuple(args.lanes.split(",")))
    if res["check"][0] != 0:
        rep.failed.append("check refused:\n" + res["check"][1][-3000:] + res["check"][2][-3000:])
        rep.finish()
    for lane in ("interp", "build", "asan"):
        for code, out, err in res.get(lane, []):
            if code != 0:
                rep.failed.append(f"[{lane}] exit {code}: {err[-1500:]}")
            got = out.split("\n")[:-1] if out.endswith("\n") else out.split("\n")
            p = 0
            for s, state, ops in cases:
                # Fresh pool states, as the generator made them.
                r2 = random.Random()
                r2.setstate(state)
                pools = [P(r2, 1000), P(r2, 2000)]
                try:
                    p += check(got[p:], ops, pools, f"c{s}.")
                    rep.passed += 1
                except AssertionError as e:
                    rep.failed.append(f"[{lane}] case {s}: {e}")
                    break
            else:
                if p < len(got):
                    rep.failed.append(f"[{lane}] {len(got) - p} lines after the last case")
    rep.finish()


if __name__ == "__main__":
    main()
