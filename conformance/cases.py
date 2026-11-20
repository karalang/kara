"""Runs a directory of fixed conformance cases: each `<name>.kara` with its
expectation, written from the page it checks (the oracle is the page).

- `<name>.out`: the program's exact stdout, on the interpreter and compiled.
- `<name>.err`: its exact stderr, when the page says what goes there.
- `<name>.unordered`: present when the page leaves the order of lines open
  (records from concurrent tasks); the lines are compared sorted.
- `<name>.repeat`: a schedule-sensitive case. The file holds the number of
  runs (20 if empty), and every run on every backend must meet the
  expectation, not just one.
- `<name>.exit`: the exit status the page promises when it is not 0 (a
  panic), and `<name>.err` then holds lines stderr must contain.
- `<name>.refused`: `karac check` must refuse it and print every line of
  this file somewhere in its output.
- `<name>.env` / `<name>.args`: a Python literal (a dict of variables to
  add to the environment, a list of command-line arguments); a bytes value
  is passed as those bytes, for a non-UTF-8 one.
- `<name>.fixed`: `karac fix` must turn the program into exactly this text.
- `<name>.buildrefused`: the page refuses it when the program is built, not
  when it is checked: `karac check` passes, and running it on the
  interpreter and building it must both fail, printing every line of this
  file on stderr.

    python3 conformance/cases.py conformance/log conformance/pool
"""

import ast
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from harness import KARAC, Report, run


def main():
    rep = Report("cases")
    for d in sys.argv[1:]:
        for src in sorted(Path(d).glob("*.kara")):
            text = src.read_text()
            fixed = src.with_suffix(".fixed")
            if fixed.exists():
                with tempfile.TemporaryDirectory(prefix="kara-conf-") as td:
                    copy = Path(td) / "main.kara"
                    shutil.copy(src, copy)
                    subprocess.run([KARAC, "fix", str(copy)], capture_output=True, text=True, timeout=120)
                    if copy.read_text() == fixed.read_text():
                        rep.passed += 1
                    else:
                        rep.failed.append(f"{src}: karac fix wrote:\n{copy.read_text()}--- want:\n{fixed.read_text()}")
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
            brefused = src.with_suffix(".buildrefused")
            if brefused.exists():
                res = run(text, timeout=120)
                for backend in ("interp", "build"):
                    for code, out, err in res.get(backend, []):
                        missing = [l for l in brefused.read_text().splitlines() if l and l not in err]
                        if code == 0 or missing:
                            rep.failed.append(f"{src} [{backend}] exit {code}, want a refusal naming {missing}:\n{err}")
                        else:
                            rep.passed += 1
                continue
            rfile = src.with_suffix(".repeat")
            repeat = int(rfile.read_text().strip() or 20) if rfile.exists() else 1
            envf, argsf = src.with_suffix(".env"), src.with_suffix(".args")
            res = run(text, repeat=repeat, timeout=120,
                      env=ast.literal_eval(envf.read_text()) if envf.exists() else None,
                      args=ast.literal_eval(argsf.read_text()) if argsf.exists() else None)
            if res["check"][0] != 0:
                rep.failed.append(f"{src}: check refused it:\n{res['check'][1]}{res['check'][2]}")
                continue
            out_file = src.with_suffix(".out")
            want_out = out_file.read_text() if out_file.exists() else ""
            err_file = src.with_suffix(".err")
            exit_file = src.with_suffix(".exit")
            want_exit = int(exit_file.read_text().strip()) if exit_file.exists() else 0
            unordered = src.with_suffix(".unordered").exists()

            def same(a, b):
                return sorted(a.splitlines()) == sorted(b.splitlines()) if unordered else a == b

            for backend in ("interp", "build"):
                for i, (code, out, err) in enumerate(res.get(backend, [])):
                    tag = f"{src} [{backend} run {i + 1}/{repeat}]"
                    if code != want_exit:
                        rep.failed.append(f"{tag} exit {code}, want {want_exit}: {err[-1500:]}")
                    elif not same(out, want_out):
                        rep.failed.append(f"{tag} stdout:\n{out}--- want:\n{want_out}")
                    elif err_file.exists() and want_exit != 0 and not all(
                            l in err for l in err_file.read_text().splitlines() if l):
                        rep.failed.append(f"{tag} stderr lacks a line of {err_file}:\n{err}")
                    elif err_file.exists() and want_exit == 0 and not same(err, err_file.read_text()):
                        rep.failed.append(f"{tag} stderr:\n{err}--- want:\n{err_file.read_text()}")
                    else:
                        rep.passed += 1
    rep.finish()


if __name__ == "__main__":
    main()
