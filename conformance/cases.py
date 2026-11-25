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
- `<name>.stdin`: bytes fed to the program's standard input on every run.
  Each run starts in a fresh, empty working directory, so a program that
  writes relative paths sees none of another run's files.
- `<name>.env` / `<name>.args`: a Python literal (a dict of variables to
  add to the environment, a list of command-line arguments); a bytes value
  is passed as those bytes, for a non-UTF-8 one.
- `<name>.fixed`: `karac fix` must turn the program into exactly this text.
- `<name>.buildrefused`: the page refuses it when the program is built, not
  when it is checked: `karac check` passes, and running it on the
  interpreter and building it must both fail, printing every line of this
  file on stderr.
- `<name>.effects`: the effect sets `karac query effects` must infer, one
  function to a line: `f = a, b` is the exact set (`f =` for none),
  `f has a, b` names atoms it must hold, `f lacks a, b` atoms it must not.
  An atom is written as `writes(Stdout)` or `panics`.
- `<name>.absent`: lines that must NOT appear in `karac check`'s output
  (a lint that must stay quiet, a retired code), beside any other kind.
- `<name>.notes`: `karac check` must accept it and print every line of this
  file (a note or warning the page promises); beside any other kind.
- `<name>.cmd`: its first line is karac's arguments, run beside the program
  as `main.kara` (`{}` stands for that name, which is otherwise added
  last, and `{-}` keeps it off a command that takes no file, such as
  `explain`), and every further line text its output must hold; beside
  any other kind.
- `<name>.json`: its first line is karac's arguments (as in `.cmd`), and
  every further line a Python expression over `d`, the parsed stdout, that
  must be true.

A case with `.effects`, `.cmd` or `.json` and neither `.out` nor `.refused`
is static: it is not run.

    python3 conformance/cases.py conformance/log conformance/pool

`CONF_LANES` names the backends a running case is held to, comma-separated:
`interp,build` by default; `asan` and `tsan` add the sanitizer builds
(`karac __mir-llvm-asan` / `__mir-llvm-tsan`, TSan on four workers and at
least three runs), and a sanitizer report fails the run with its first line.
"""

import ast
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from harness import KARAC, Report, run


LANES = tuple(os.environ.get("CONF_LANES", "interp,build").split(","))


def report_line(err: str) -> str:
    m = re.search(r"(?:ERROR|WARNING): (?:Address|Leak|Thread)Sanitizer: [^\n]*", err)
    return m.group(0)[:200] if m else ""


def query_effects(text):
    """{function: set of atoms} from `karac query effects`, or None with the
    output when it did not answer."""
    with tempfile.TemporaryDirectory(prefix="kara-conf-") as td:
        path = Path(td) / "main.kara"
        path.write_text(text)
        p = subprocess.run([KARAC, "query", "effects", str(path)], capture_output=True, text=True, timeout=120)
    try:
        doc = json.loads(p.stdout)
    except ValueError:
        return None, p.stdout + p.stderr
    return {f["function"]: {e["verb"] + (f"({e['resource']})" if e["resource"] else "")
                            for e in f["inferred_effects"]} for f in doc.get("functions", [])}, ""


def check_effects(src, text, rep):
    got, raw = query_effects(text)
    if got is None:
        rep.failed.append(f"{src}: karac query effects did not answer:\n{raw}")
        return
    for line in src.with_suffix(".effects").read_text().splitlines():
        m = re.fullmatch(r"\s*(\S+)\s+(=|has|lacks)\s*(.*)", line)
        if not m:
            continue
        fn, op, rest = m.groups()
        atoms = {a.strip() for a in re.split(r",\s*(?![^()]*\))", rest) if a.strip()}
        have = got.get(fn)
        if have is None:
            rep.failed.append(f"{src}: no effect entry for {fn}; have {sorted(got)}")
        elif (op == "=" and have != atoms) or (op == "has" and not atoms <= have) or (op == "lacks" and have & atoms):
            rep.failed.append(f"{src}: {fn} has {sorted(have)}, want {line.strip()!r}")
        else:
            rep.passed += 1


def karac_beside(text, args):
    """karac run with `args` in a directory holding the program as
    `main.kara`; `{}` in an argument names it, and it is added last
    otherwise, unless a `{-}` argument says the command takes no file."""
    argv = shlex.split(args)
    if "{-}" in argv:
        argv.remove("{-}")
    elif not any("{}" in a for a in argv):
        argv.append("{}")
    with tempfile.TemporaryDirectory(prefix="kara-conf-") as td:
        (Path(td) / "main.kara").write_text(text)
        return subprocess.run([KARAC, *(a.replace("{}", "main.kara") for a in argv)], capture_output=True,
                              text=True, timeout=300, cwd=td)


def check_cmd(src, text, rep):
    lines = src.with_suffix(".cmd").read_text().splitlines()
    p = karac_beside(text, lines[0])
    for line in lines[1:]:
        if line and line not in p.stdout + p.stderr:
            rep.failed.append(f"{src}: `karac {lines[0]}` output lacks {line!r}:\n{p.stdout}{p.stderr}")
        elif line:
            rep.passed += 1


def check_json(src, text, rep):
    lines = [l for l in src.with_suffix(".json").read_text().splitlines() if l.strip()]
    p = karac_beside(text, lines[0])
    try:
        d = json.loads(p.stdout)
    except ValueError:
        rep.failed.append(f"{src}: karac {lines[0]} printed no JSON:\n{p.stdout}{p.stderr}")
        return
    for expr in lines[1:]:
        try:
            ok = bool(eval(expr, {"d": d}))
        except Exception as e:  # a missing key is a failure, not a crash
            ok, expr = False, f"{expr} ({e!r})"
        if ok:
            rep.passed += 1
        else:
            rep.failed.append(f"{src}: not true of `karac {lines[0]}`: {expr}\n{json.dumps(d)[:1500]}")


def main():
    rep = Report("cases")
    for d in sys.argv[1:]:
        for src in sorted(Path(d).glob("*.kara")):
            text = src.read_text()
            absent = src.with_suffix(".absent")
            if absent.exists():
                code, out, err = run(text, backends=())["check"]
                for line in absent.read_text().splitlines():
                    if line and line in out + err:
                        rep.failed.append(f"{src}: check output has {line!r}:\n{out}{err}")
                    elif line:
                        rep.passed += 1
            notes = src.with_suffix(".notes")
            if notes.exists():
                code, out, err = run(text, backends=())["check"]
                for line in notes.read_text().splitlines():
                    if line and (code != 0 or line not in out + err):
                        rep.failed.append(f"{src}: check exit {code}, want 0 and {line!r}:\n{out}{err}")
                    elif line:
                        rep.passed += 1
            if src.with_suffix(".cmd").exists():
                check_cmd(src, text, rep)
            if src.with_suffix(".json").exists():
                check_json(src, text, rep)
            if src.with_suffix(".effects").exists():
                check_effects(src, text, rep)
            static = any(src.with_suffix(k).exists() for k in (".json", ".cmd", ".effects"))
            if static and not src.with_suffix(".out").exists() and not src.with_suffix(".refused").exists():
                continue
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
            stdinf = src.with_suffix(".stdin")
            res = run(text, backends=LANES, repeat=repeat, timeout=300,
                      stdin=stdinf.read_text(errors="surrogateescape") if stdinf.exists() else "",
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

            for backend in LANES:
                runs = res.get(backend, [])
                for i, (code, out, err) in enumerate(runs):
                    tag = f"{src} [{backend} run {i + 1}/{len(runs)}]"
                    if report_line(err):
                        rep.failed.append(f"{tag} {report_line(err)}")
                    elif code != want_exit:
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
