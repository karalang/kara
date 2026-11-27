"""docs/library/concurrency.md § Channels (`select`, `try_send`, `try_recv`,
closing), against a model of the page's rules. Random scripts in one task,
so every result is determined: a few channels of a `Drop` type `Tok` whose
drop prints its id, `select`s over random recv and send arms (with
`default` whenever no arm is ready, and sometimes a far `after`), and
between them `try_send`, `try_recv` and `drop` of either end. Held to the
model: when several arms are ready the first in source order wins; only
the chosen arm's operation happens; a losing send arm's value drops when
the `select` ends; a closed channel makes its arm ready (`recv` gives
`None` once drained, `send` gives `Closed(v)`); `try_send` gives `Full(v)`
at capacity; messages come out in order; dropping the `Receiver` drops
the buffered messages. Drops that happen together (one `select`'s
leftovers, a dropped receiver's buffer, the end of `main`) are compared as
a set, since the page fixes no order among them.

    python3 conformance/select_model.py [--seed N] [--scripts N] [--steps N] [--lanes interp,build,asan]
"""

import argparse
import random

from harness import Report, run

HEAD = """import std.sync.{channel, Sender, Receiver, SendError};
import std.time.{Duration};

struct Tok { id: i64 }

impl Drop for Tok {
    fn drop(mut ref self) {
        println(f"drop {self.id}");
    }
}

"""


class Chan:
    def __init__(self, cap):
        self.cap, self.buf, self.tx, self.rx = cap, [], True, True


def script(rng, n_steps, tag):
    """(kara function body, expected items): an item is a line, or a
    frozenset of lines that may come in any order."""
    chans = [Chan(rng.choice([1, 1, 2, 3])) for _ in range(rng.randrange(1, 4))]
    lines = []
    for i, c in enumerate(chans):
        lines.append(f"    let (t{i}, r{i}): (Sender[Tok], Receiver[Tok]) = channel({c.cap});")
    want = []
    nxt = [100]

    def new_id():
        nxt[0] += 1
        return nxt[0]

    for k in range(n_steps):
        live_tx = [i for i, c in enumerate(chans) if c.tx]
        live_rx = [i for i, c in enumerate(chans) if c.rx]
        r = rng.random()
        if r < 0.15 and live_tx:
            i = rng.choice(live_tx)
            n = new_id()
            lines.append(f"    match t{i}.try_send(Tok {{ id: {n} }}) {{\n"
                         f'        Ok(_) => println("{tag}{k} ts{i} ok"),\n'
                         f'        Err(SendError.Full(x)) => println(f"{tag}{k} ts{i} full {{x.id}}"),\n'
                         f'        Err(SendError.Closed(x)) => println(f"{tag}{k} ts{i} closed {{x.id}}"),\n'
                         "    }")
            c = chans[i]
            if not c.rx:
                want += [f"{tag}{k} ts{i} closed {n}", f"drop {n}"]
            elif len(c.buf) >= c.cap:
                want += [f"{tag}{k} ts{i} full {n}", f"drop {n}"]
            else:
                c.buf.append(n)
                want.append(f"{tag}{k} ts{i} ok")
            continue
        if r < 0.27 and live_rx:
            i = rng.choice(live_rx)
            lines.append(f"    match r{i}.try_recv() {{\n"
                         f'        Some(x) => println(f"{tag}{k} tr{i} {{x.id}}"),\n'
                         f'        None => println("{tag}{k} tr{i} none"),\n'
                         "    }")
            c = chans[i]
            if c.buf:
                n = c.buf.pop(0)
                want += [f"{tag}{k} tr{i} {n}", f"drop {n}"]
            else:
                want.append(f"{tag}{k} tr{i} none")
            continue
        if r < 0.33 and len(live_tx) + len(live_rx) > 1:
            side, i = rng.choice([("t", i) for i in live_tx] + [("r", i) for i in live_rx])
            lines.append(f"    drop({side}{i});")
            lines.append(f'    println("{tag}{k} dropped {side}{i}");')
            c = chans[i]
            if side == "t":
                c.tx = False
                want.append(f"{tag}{k} dropped t{i}")
            else:
                c.rx = False
                if c.buf:
                    want.append(frozenset(f"drop {n}" for n in c.buf))
                c.buf = []
                want.append(f"{tag}{k} dropped r{i}")
            continue
        # A select over 1 to 4 arms.
        arms = []
        for _ in range(rng.randrange(1, 5)):
            opts = [("recv", i) for i in live_rx] + [("send", i) for i in live_tx]
            if not opts:
                break
            arms.append(rng.choice(opts))
        if not arms:
            continue
        ids = [new_id() if a == "send" else None for a, _ in arms]

        def ready(a, i):
            c = chans[i]
            if a == "recv":
                return bool(c.buf) or not c.tx
            return not c.rx or len(c.buf) < c.cap

        any_ready = any(ready(a, i) for a, i in arms)
        far = rng.random() < 0.3
        default = not any_ready or rng.random() < 0.4
        body = []
        for j, ((a, i), n) in enumerate(zip(arms, ids)):
            if a == "recv":
                body.append(f"        a{j} = r{i}.recv() => match a{j} {{\n"
                            f'            Some(x) => println(f"{tag}{k} arm{j} r{i} {{x.id}}"),\n'
                            f'            None => println("{tag}{k} arm{j} r{i} none"),\n'
                            "        },")
            else:
                body.append(f"        s{j} = t{i}.send(Tok {{ id: {n} }}) => match s{j} {{\n"
                            f'            Ok(_) => println("{tag}{k} arm{j} t{i} ok"),\n'
                            f'            Err(SendError.Closed(x)) => println(f"{tag}{k} arm{j} t{i} closed {{x.id}}"),\n'
                            f'            Err(SendError.Full(x)) => println(f"{tag}{k} arm{j} t{i} full {{x.id}}"),\n'
                            "        },")
        if far:
            body.append(f'        _ = after(Duration.ms(600000)) => println("{tag}{k} after"),')
        if default:
            body.append(f'        default => println("{tag}{k} default"),')
        lines.append("    select {\n" + "\n".join(body) + "\n    }")
        lines.append(f'    println("{tag}{k} end");')
        drops = set()
        won = next((j for j, (a, i) in enumerate(arms) if ready(a, i)), None)
        if won is None:
            want.append(f"{tag}{k} default")
        else:
            a, i = arms[won]
            c = chans[i]
            if a == "recv":
                if c.buf:
                    n = c.buf.pop(0)
                    want.append(f"{tag}{k} arm{won} r{i} {n}")
                    drops.add(f"drop {n}")
                else:
                    want.append(f"{tag}{k} arm{won} r{i} none")
            elif not c.rx:
                want.append(f"{tag}{k} arm{won} t{i} closed {ids[won]}")
                drops.add(f"drop {ids[won]}")
            else:
                c.buf.append(ids[won])
                want.append(f"{tag}{k} arm{won} t{i} ok")
        for j, n in enumerate(ids):
            if n is not None and j != won:
                drops.add(f"drop {n}")
        if drops:
            want.append(frozenset(drops))
        want.append(f"{tag}{k} end")
    lines.append(f'    println("{tag} done");')
    want.append(f"{tag} done")
    rest = frozenset(f"drop {n}" for c in chans for n in c.buf)
    if rest:
        want.append(rest)
    return "\n".join(lines), want


def match(got, want):
    """The first difference between `got` (lines) and `want` (items), or None."""
    p = 0
    for item in want:
        if isinstance(item, frozenset):
            part = got[p:p + len(item)]
            if set(part) != item or len(part) != len(item):
                return f"at line {p}: got {part!r}, want in any order {sorted(item)!r}"
            p += len(item)
        else:
            if p >= len(got) or got[p] != item:
                return f"at line {p}: got {got[p] if p < len(got) else 'end of output'!r}, want {item!r}"
            p += 1
    if p != len(got):
        return f"{len(got) - p} extra lines from line {p}: {got[p:p + 3]!r}"
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--scripts", type=int, default=40)
    ap.add_argument("--steps", type=int, default=25)
    ap.add_argument("--lanes", default="interp,build")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("select_model")
    fns, want, calls = [], [], []
    for s in range(args.scripts):
        body, w = script(rng, args.steps, f"c{s}.")
        fns.append(f"fn case{s}() {{\n{body}\n}}\n")
        calls.append(f"    case{s}();")
        want.append((s, w))
    src = HEAD + "\n".join(fns) + "\nfn main() {\n" + "\n".join(calls) + "\n}\n"
    res = run(src, backends=tuple(args.lanes.split(",")))
    if res["check"][0] != 0:
        rep.failed.append("check refused:\n" + res["check"][1][-3000:] + res["check"][2][-3000:])
        rep.finish()
    for lane in ("interp", "build", "asan"):
        for code, out, err in res.get(lane, []):
            if code != 0:
                rep.failed.append(f"[{lane}] exit {code}: {err[-1500:]}")
            got = out.split("\n")[:-1] if out.endswith("\n") else out.split("\n")
            # Split the output at each case's last line, `c<s>. done` plus its drops.
            p = 0
            for s, w in want:
                n = sum(len(i) if isinstance(i, frozenset) else 1 for i in w)
                why = match(got[p:p + n], w)
                if why:
                    rep.failed.append(f"[{lane}] case {s} {why}")
                else:
                    rep.passed += 1
                p += n
            if p < len(got):
                rep.failed.append(f"[{lane}] {len(got) - p} lines after the last case")
    rep.finish()


if __name__ == "__main__":
    main()
