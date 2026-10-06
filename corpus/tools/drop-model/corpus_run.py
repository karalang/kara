#!/usr/bin/env python3
"""Run the source-level drop model (kmodel.py) over corpus programs.

    python3 corpus/tools/drop-model/corpus_run.py [--tag drop] [--path fixtures/] [--json out.json]

Each program gets one verdict, compared with its recorded legacy run:

  SAME           the model's stdout and exit code equal legacy.out
  ORDER          same lines, different order (the class-c shape)
  ORDER-UNSPEC   same lines, and the program dropped Map/Set elements, whose order §7.8 leaves open
  DIFF           different lines: a legacy bug v2 fixes, a model bug, or a fixture pinning a legacy bug
  V2-REJECT      the program breaks a v2 rule on the path the model executed (class b)
  LEGACY-REJECT  legacy refused the program and the model ran it
  UNSUP / PARSE  outside the model's subset: no verdict
  CRASH          a model bug

The model checks rules only on the executed path, so V2-REJECT is a lower
bound on what `karac check` must reject, never an upper one.
"""
import argparse, collections as C, json, os, signal, sys, tomllib, traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from kmodel import ModelError, Unsupported, run_source  # noqa: E402
from kparse import ParseError  # noqa: E402

ROOT = Path(os.environ.get("KARA_ROOT", HERE.parents[2]))


class Timeout(Exception):
    pass


def _alarm(*_):
    raise Timeout()


def verdict(d: Path, meta: dict):
    legacy = (d / "legacy.out").read_text() if (d / "legacy.out").exists() else ""
    lexit = meta.get("legacy_exit", 0)
    signal.alarm(int(os.environ.get("MODEL_TIMEOUT", "20")))
    try:
        out, code, flags = run_source((d / "source.kara").read_text())
    except ModelError as e:
        return "V2-REJECT", str(e), None
    except Unsupported as e:
        return "UNSUP", str(e), None
    except ParseError as e:
        return "PARSE", str(e), None
    except Timeout:
        return "UNSUP", "model timeout", None
    except Exception:
        return "CRASH", traceback.format_exc().splitlines()[-1], None
    finally:
        signal.alarm(0)
    if meta.get("expect", "").startswith("error"):
        return "LEGACY-REJECT", f"model ran, exit {code}", out
    if out == legacy and code == lexit:
        return "SAME", "", out
    if code == lexit and sorted(out.splitlines()) == sorted(legacy.splitlines()):
        return ("ORDER-UNSPEC" if "unordered" in flags else "ORDER"), "", out
    if "narrow-int" in flags and code != lexit and 101 in (code, lexit):
        return "UNSUP", "narrow integer width (the model's integers are i64)", None
    why = f"exit model {code} legacy {lexit}"
    panics = sorted(f for f in flags if f.startswith("panic: "))
    if panics:
        why += f" ({panics[0]})"
    return "DIFF", why, out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", help="only programs carrying this tag")
    ap.add_argument("--path", help="only programs whose corpus path contains this")
    ap.add_argument("--json", help="write one row per program here")
    ap.add_argument("--include-v2", action="store_true", help="also run core/ and drop-matrix/")
    a = ap.parse_args()
    signal.signal(signal.SIGALRM, _alarm)
    rows = []
    for mp in sorted((ROOT / "corpus").glob("**/meta.toml")):
        d = mp.parent
        rel = str(d.relative_to(ROOT))
        if not a.include_v2 and ("/core/" in rel + "/" or "/drop-matrix/" in rel):
            continue
        if a.path and a.path not in rel:
            continue
        meta = tomllib.loads(mp.read_text())
        if meta.get("expect") == "skip" or (a.tag and a.tag not in meta.get("tags", [])):
            continue
        v, why, _ = verdict(d, meta)
        rows.append({"dir": rel, "verdict": v, "why": why, "tags": meta.get("tags", [])})
    if a.json:
        Path(a.json).write_text(json.dumps(rows, indent=0))
    if not rows:
        print("no program matched", file=sys.stderr)
        sys.exit(2)
    c = C.Counter(r["verdict"] for r in rows)
    print(f"{len(rows)} programs: " + ", ".join(f"{k} {n}" for k, n in c.most_common()))
    for k in ("CRASH", "DIFF", "V2-REJECT", "UNSUP", "PARSE"):
        cc = C.Counter(r["why"][:80] for r in rows if r["verdict"] == k)
        if cc:
            print(f"--- {k}")
            for w, n in cc.most_common(10):
                print(f"  {n:5} {w}")


if __name__ == "__main__":
    main()
