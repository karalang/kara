"""Runs a directory of fixed conformance cases: each `<name>.kara` with its
expectation, written from the page it checks (the oracle is the page).

- `<name>.out`: the program's exact stdout, on the interpreter and compiled.
- `<name>.err`: its exact stderr, when the page says what goes there.
- `<name>.unordered`: present when the page leaves the order of lines open
  (records from concurrent tasks); the lines are compared sorted.
- `<name>.refused`: `karac check` must refuse it and print every line of
  this file somewhere in its output.

    python3 conformance/cases.py conformance/log conformance/pool
"""

import sys
from pathlib import Path

from harness import Report, run


def main():
    rep = Report("cases")
    for d in sys.argv[1:]:
        for src in sorted(Path(d).glob("*.kara")):
            text = src.read_text()
            refused = src.with_suffix(".refused")
            if refused.exists():
                res = run(text, backends=())
                code, out, err = res["check"]
                if code == 0:
                    rep.failed.append(f"{src}: check accepted it")
                    continue
                for line in refused.read_text().splitlines():
                    if line and line not in out + err:
                        rep.failed.append(f"{src}: check output lacks {line!r}:\n{out}{err}")
                    elif line:
                        rep.passed += 1
                continue
            res = run(text)
            if res["check"][0] != 0:
                rep.failed.append(f"{src}: check refused it:\n{res['check'][1]}{res['check'][2]}")
                continue
            want_out = src.with_suffix(".out").read_text()
            err_file = src.with_suffix(".err")
            unordered = src.with_suffix(".unordered").exists()

            def same(a, b):
                return sorted(a.splitlines()) == sorted(b.splitlines()) if unordered else a == b
            for backend in ("interp", "build"):
                if backend not in res:
                    continue
                code, out, err = res[backend]
                if code != 0:
                    rep.failed.append(f"{src} [{backend}] exit {code}: {err[-1500:]}")
                elif not same(out, want_out):
                    rep.failed.append(f"{src} [{backend}] stdout:\n{out}--- want:\n{want_out}")
                elif err_file.exists() and not same(err, err_file.read_text()):
                    rep.failed.append(f"{src} [{backend}] stderr:\n{err}--- want:\n{err_file.read_text()}")
                else:
                    rep.passed += 1
    rep.finish()


if __name__ == "__main__":
    main()
