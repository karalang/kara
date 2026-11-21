#!/usr/bin/env python3
"""concurrency.md's channel rules against Go's buffered channels.

Random scripts of channel operations run in one task, so every result is
determined: `try_send`, `try_recv`, `send` and `recv` where they cannot
wait, dropping one `Sender` clone, cloning one, and dropping the `Receiver`.
The expected results come from a model of the page's rules, and the model
is itself checked against Go: the scripts that never drop the receiver are
compiled as Go programs (a `select` with `default` for each try, `close`
when the last sender goes) and Go's output must equal the model's. Where
the page differs from Go on purpose, the model follows the page: a send
with no receiver left returns `Closed(v)` instead of Go's panic (Go cannot
drop a receiver at all), and `try_recv` gives `None` both when empty and
when closed.

    python3 conformance/channel_model.py [--seed N] [--scripts N]

`GO=<path>` picks the go binary (default `go` on PATH); without one the
model is not cross-checked, and the script says so.
"""

import argparse
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import Report, run  # noqa: E402


def gen(rng, n_ops):
    """A script: a capacity and a list of ops that never wait."""
    cap = rng.choice([1, 1, 2, 3, 5])
    ops, buf, senders, rx, nxt = [], 0, 1, True, 1
    allow_rx_drop = rng.random() < 0.4
    for _ in range(n_ops):
        choices = ["try_send", "try_recv", "try_send", "try_recv"]
        if senders and (buf < cap or not rx):
            choices.append("send")
        if rx and (buf > 0 or senders == 0):
            choices.append("recv")
        if senders:
            choices += ["clone"] if senders < 3 else []
            choices += ["drop_tx"]
        if rx and allow_rx_drop:
            choices.append("drop_rx")
        op = rng.choice(choices)
        if op in ("try_send", "send") and not senders:
            op = "try_recv" if rx else None
        if op in ("try_recv", "recv") and not rx:
            op = "try_send" if senders else None
        if op is None:
            break
        if op in ("try_send", "send"):
            ops.append((op, nxt))
            nxt += 1
            if rx and buf < cap:
                buf += 1
        elif op in ("try_recv", "recv"):
            ops.append((op, None))
            if buf:
                buf -= 1
        elif op == "clone":
            ops.append((op, None))
            senders += 1
        elif op == "drop_tx":
            ops.append((op, None))
            senders -= 1
        elif op == "drop_rx":
            ops.append((op, None))
            rx = False
            buf = 0
    return cap, ops


def model(cap, ops):
    """The page's results, one line per op that reports one."""
    q, senders, rx, out = [], 1, True, []
    for op, v in ops:
        if op in ("try_send", "send"):
            if not rx:
                out.append(f"closed {v}")
            elif len(q) < cap:
                q.append(v)
                out.append("ok")
            else:  # only try_send reaches here: a send is generated only where it fits
                out.append(f"full {v}")
        elif op in ("try_recv", "recv"):
            out.append(f"got {q.pop(0)}" if q else "none")
        elif op == "clone":
            senders += 1
        elif op == "drop_tx":
            senders -= 1
        elif op == "drop_rx":
            rx = False
            q.clear()
    return out


def kara_fn(i, cap, ops):
    lines = [f"fn script{i}() {{",
             f"    let (tx, rx): (Sender[i64], Receiver[i64]) = channel({cap});",
             "    let mut txs: Vec[Sender[i64]] = [tx];",
             "    let mut rxo: Option[Receiver[i64]] = Some(rx);"]
    for op, v in ops:
        if op in ("try_send", "send"):
            call = "try_send" if op == "try_send" else "send"
            lines.append(f"    match txs[txs.len() - 1].{call}({v}) {{ Ok(()) => println(\"ok\"), "
                         f"Err(SendError.Full(x)) => println(f\"full {{x}}\"), "
                         f"Err(SendError.Closed(x)) => println(f\"closed {{x}}\") }}")
        elif op in ("try_recv", "recv"):
            call = "try_recv" if op == "try_recv" else "recv"
            lines.append(f"    match rxo {{ Some(ref r) => match r.{call}() {{ Some(x) => println(f\"got {{x}}\"), "
                         f"None => println(\"none\") }}, None => {{}} }}")
        elif op == "clone":
            lines.append("    let c = txs[0].clone();")
            lines.append("    txs.push(c);")
        elif op == "drop_tx":
            lines.append("    let _ = txs.pop();")
        elif op == "drop_rx":
            lines.append("    rxo = None;")
    lines.append(f"    println(\"end {i}\");")
    lines.append("}")
    return "\n".join(lines)


def go_fn(i, cap, ops):
    lines = [f"func script{i}() {{", f"\tch := make(chan int64, {cap})", "\tsenders := 1"]
    for op, v in ops:
        if op == "try_send":
            lines.append(f"\tselect {{ case ch <- {v}: fmt.Println(\"ok\"); default: fmt.Println(\"full {v}\") }}")
        elif op == "send":
            lines.append(f"\tch <- {v}; fmt.Println(\"ok\")")
        elif op == "try_recv":
            lines.append("\tselect { case x, ok := <-ch: if ok { fmt.Printf(\"got %d\\n\", x) } else "
                         "{ fmt.Println(\"none\") }; default: fmt.Println(\"none\") }")
        elif op == "recv":
            lines.append("\tif x, ok := <-ch; ok { fmt.Printf(\"got %d\\n\", x) } else { fmt.Println(\"none\") }")
        elif op == "clone":
            lines.append("\tsenders++")
        elif op == "drop_tx":
            lines.append("\tsenders--; if senders == 0 { close(ch) }")
    lines.append(f"\t_ = senders; fmt.Println(\"end {i}\")")
    lines.append("}")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--scripts", type=int, default=120)
    a = ap.parse_args()
    rng = random.Random(a.seed)
    rep = Report("channel_model")
    scripts = [gen(rng, rng.randrange(4, 30)) for _ in range(a.scripts)]

    want = []
    for i, (cap, ops) in enumerate(scripts):
        want += model(cap, ops) + [f"end {i}"]

    # The model against Go, on the scripts Go can express.
    go = os.environ.get("GO") or shutil.which("go")
    goable = [(i, c, o) for i, (c, o) in enumerate(scripts) if not any(op == "drop_rx" for op, _ in o)]
    if go:
        src = "package main\n\nimport \"fmt\"\n\n" + "\n\n".join(go_fn(i, c, o) for i, c, o in goable)
        src += "\n\nfunc main() {\n" + "".join(f"\tscript{i}()\n" for i, _, _ in goable) + "}\n"
        with tempfile.TemporaryDirectory(prefix="kara-go-") as d:
            (Path(d) / "main.go").write_text(src)
            r = subprocess.run([go, "run", "main.go"], cwd=d, capture_output=True, text=True, timeout=600)
        go_lines = r.stdout.splitlines()
        model_lines = []
        for i, c, o in goable:
            model_lines += model(c, o) + [f"end {i}"]
        if r.returncode != 0:
            rep.failed.append(f"go run failed: {r.stderr[-1500:]}")
        elif go_lines != model_lines:
            bad = next(k for k, (x, y) in enumerate(zip(go_lines + [""], model_lines + [""])) if x != y)
            rep.failed.append(f"model disagrees with Go at line {bad}: go {go_lines[bad:bad + 3]} "
                              f"model {model_lines[bad:bad + 3]}")
        else:
            print(f"model == Go on {len(goable)} scripts ({len(model_lines)} lines)")
    else:
        print("no go binary: the model is not cross-checked against Go")

    src = "import std.sync.{channel, Sender, Receiver, SendError};\n\n"
    src += "\n\n".join(kara_fn(i, c, o) for i, (c, o) in enumerate(scripts))
    src += "\n\nfn main() {\n" + "".join(f"    script{i}();\n" for i in range(len(scripts))) + "}\n"
    lanes = tuple(os.environ.get("CONF_LANES", "interp,build").split(","))
    res = run(src, backends=lanes, timeout=900)
    if res["check"][0] != 0:
        rep.failed.append("check refused the program:\n" + (res["check"][1] + res["check"][2])[-3000:])
        rep.finish()
    for backend in lanes:
        for code, out, err in res.get(backend, []):
            if code != 0:
                rep.failed.append(f"[{backend}] exit {code}: {err[-1500:]}")
            rep.compare(backend, [f"line {k}" for k in range(len(want))], out.splitlines(), want)
    rep.finish()


if __name__ == "__main__":
    main()
