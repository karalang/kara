"""Runs a directory of package conformance cases, for the pages whose promises
are about a package rather than one file: modules and imports, visibility,
the manifest, targets and build artifacts (design.md §4, §16). Each case is
a directory `<name>/` holding a package tree, copied to a scratch directory
and run there, with its expectation in files beside it (the oracle is the
page):

- `<name>.cmd`: a Python literal, a list of runs; each run is a list of
  steps, each step an argv. A step whose first word starts with `./` runs
  that file from the package directory, and one whose first word starts
  with `@` runs that tool from PATH (`@objdump`); any other step is a
  `karac` command. Every run must meet the expectation, which is read on its last
  step; an earlier step must exit 0. Without the file, a package with
  `src/main.kara` runs twice: `karac run --interp`, and `karac build` then
  `./dist/native/<name>`; a package without one is checked
  (`karac check`). A run whose steps include `build` is skipped when the
  karac binary has no LLVM backend.
- `<name>.out`: the last step's exact stdout.
- `<name>.exit`: the last step's exit status, when it is not 0.
- `<name>.err`: lines the last step must print, on stdout or stderr.
- `<name>.noterr`: lines it must not print.
- `<name>.files`: a Python literal dict, path to whether it must exist in
  the package directory after the run.
- `<name>.env`: a Python literal dict of variables to add to the
  environment.
- `<name>.cwd`: the directory, relative to the package, to run from.
- `<name>.host`: the host the case needs, as `<machine>-<system>`
  (`x86_64-linux`); on another host it is skipped.

    python3 conformance/packages.py conformance/design/modules
"""

import ast
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

from harness import KARAC, Report, has_backend


def default_runs(pkg: Path):
    if not (pkg / "src" / "main.kara").exists():
        return [[["check"]]]
    name = tomllib.loads((pkg / "kara.toml").read_text())["package"]["name"]
    return [[["run", "--interp"]], [["build"], [f"./dist/native/{name}"]]]


def main():
    rep = Report("packages")
    backend = has_backend()
    for d in sys.argv[1:]:
        for case in sorted(p for p in Path(d).iterdir() if p.is_dir() and not p.name.startswith("_")):
            side = lambda ext: case.parent / (case.name + ext)
            runs = ast.literal_eval(side(".cmd").read_text()) if side(".cmd").exists() else default_runs(case)
            env = dict(os.environ, **(ast.literal_eval(side(".env").read_text()) if side(".env").exists() else {}))
            want_exit = int(side(".exit").read_text().strip()) if side(".exit").exists() else 0
            want_out = side(".out").read_text() if side(".out").exists() else None
            want_err = [l for l in side(".err").read_text().splitlines() if l] if side(".err").exists() else []
            not_err = [l for l in side(".noterr").read_text().splitlines() if l] if side(".noterr").exists() else []
            files = ast.literal_eval(side(".files").read_text()) if side(".files").exists() else {}
            host = side(".host").read_text().strip() if side(".host").exists() else None
            if host and host != f"{platform.machine()}-{platform.system().lower()}":
                continue
            for run in runs:
                if not backend and any(step[0] == "build" for step in run):
                    continue
                tag = f"{case} {' && '.join(' '.join(s) for s in run)}"
                with tempfile.TemporaryDirectory(prefix="kara-pkg-") as td:
                    pkg = Path(td) / case.name
                    shutil.copytree(case, pkg)
                    cwd = pkg / side(".cwd").read_text().strip() if side(".cwd").exists() else pkg
                    result = None
                    for i, step in enumerate(run):
                        if step[0].startswith("./"):
                            argv = [str(pkg / step[0])] + step[1:]
                        elif step[0].startswith("@"):
                            argv = [step[0][1:]] + step[1:]
                        else:
                            argv = [KARAC] + step
                        try:
                            r = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True,
                                               timeout=300, stdin=subprocess.DEVNULL)
                            result = (r.returncode, r.stdout, r.stderr)
                        except (subprocess.TimeoutExpired, OSError) as e:
                            result = (-999, "", str(e))
                        if i < len(run) - 1 and result[0] != 0:
                            break
                    code, out, err = result
                    both = out + err
                    if i < len(run) - 1:
                        rep.failed.append(f"{tag}: step {i + 1} exit {code}:\n{both[-1500:]}")
                        continue
                    problems = []
                    if code != want_exit:
                        problems.append(f"exit {code}, want {want_exit}")
                    if want_out is not None and out != want_out:
                        problems.append(f"stdout:\n{out}--- want:\n{want_out}")
                    problems += [f"lacks {l!r}" for l in want_err if l not in both]
                    problems += [f"prints {l!r}" for l in not_err if l in both]
                    for path, exists in files.items():
                        if (pkg / path).exists() != exists:
                            problems.append(f"{path} {'missing' if exists else 'present'}")
                    if problems:
                        rep.failed.append(f"{tag}: " + "; ".join(problems) + f"\n{both[-1500:]}")
                    else:
                        rep.passed += 1
    rep.finish()


if __name__ == "__main__":
    main()
