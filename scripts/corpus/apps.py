#!/usr/bin/env python3
"""Measure v2 friction on a hand-written corpus app (corpus/apps/<kind>/<name>/).

    python3 scripts/corpus/apps.py fix <dir>      # source.orig.kara -> source.kara via `karac fix`
    python3 scripts/corpus/apps.py measure <dir>  # print the friction row for the app
    python3 scripts/corpus/apps.py table corpus/apps  # re-measure all, rewrite the README table
    python3 scripts/corpus/apps.py finalize <dir> [--groups "L1-L2 ..."] [--hand "why" ...] [--pre "why" ...]

`--groups` names the stmt-par groups (implies --stmt-par; becomes the meta
note), `--hand` gives one reason per hand edit, and `--pre` records what the
drafting agent had to change before the draft built under legacy (compiler
gaps it worked around, kept in friction.json as draft_workarounds).

`fix` copies source.orig.kara (the first draft, written against the spec by
an author who had not seen the v2 errors) to source.kara and applies
`karac fix` until it reaches a fixed point. What `karac check` still reports
afterwards is left for hand edits.

`measure` compares source.orig.kara with source.kara: `.clone()` calls and
`ref` keywords added (counted from the token diff, so a hand edit that adds
one counts too), lines changed by hand (the diff between the fixed draft and
source.kara), the errors `karac check` reports on the draft by code, whether
source.kara is clean, and whether its legacy run equals the mirror's output.
"""

import collections
import difflib
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
KARAC = str(ROOT / "target/debug/karac")


KEEP = {"source.orig.kara", "source.fixed.kara", "source.kara", "notes.md", "meta.toml",
        "expected.out", "legacy.out", "model.out"}


def sweep(d, name):
    """Delete what the mirror or a legacy run wrote: anything not a corpus file."""
    for f in d.iterdir():
        if f.is_file() and f.name not in KEEP and f.name != f"{name}.py":
            f.unlink()


def check(path: Path) -> list:
    r = subprocess.run([KARAC, "check", "--output=json", path.name], cwd=path.parent,
                       capture_output=True, text=True, timeout=300)
    try:
        return [d for d in json.loads(r.stdout).get("diagnostics", []) if d.get("severity") == "error"]
    except json.JSONDecodeError:
        return [{"code": "unparsable-check-output", "message": r.stderr[:200]}]


def fix_to_fixed_point(path: Path) -> int:
    rounds = 0
    while rounds < 20:
        before = path.read_text()
        subprocess.run([KARAC, "fix", path.name], cwd=path.parent, capture_output=True, timeout=300)
        rounds += 1
        if path.read_text() == before:
            break
    return rounds


def count(src: str) -> dict:
    return {"clone": len(re.findall(r"\.clone\(\)", src)),
            "ref": len(re.findall(r"\bref\b", src)),
            "shared": len(re.findall(r"\bshared\s+(struct|enum)\b", src)),
            "derive_copy": len(re.findall(r"#\[derive\([^)]*\bCopy\b", src)),
            "into_iter": len(re.findall(r"\.into_iter\(\)", src))}


def finalize(d: Path, stmt_par: bool, hand: list, groups: str, pre: list) -> int:
    """expected.out from the same-stem Python mirror, legacy runs cross-checked,
    meta.toml written, and the friction row stored in friction.json."""
    sys.path.insert(0, str(ROOT / "scripts/corpus"))
    from corpus_lib import read_toml, run_program, write_toml
    kind, name = d.parts[-2], d.parts[-1]
    m = subprocess.run([sys.executable, f"{name}.py"], cwd=d, capture_output=True, timeout=300)
    if m.returncode != 0:
        print(f"mirror failed: {m.stderr.decode()[-300:]}")
        return 1
    sweep(d, name)
    (d / "expected.out").write_bytes(m.stdout)
    legacy = {}
    for b in ("legacy-build", "legacy-interp"):
        r = run_program(d, b, KARAC, 300, {"KARAC_HASH_SEED": "1"})
        legacy[b] = "agrees" if r["stdout"] == m.stdout and r["exit"] == 0 else \
            f"differs ({r['status']}, exit {r['exit']}, {len(r['stdout'])} vs {len(m.stdout)} bytes)"
    row = json.loads(subprocess.run([sys.executable, __file__, "measure", str(d)], capture_output=True,
                                    text=True, check=True).stdout)
    fj = d.parent.parent / "friction.json"
    rows = json.loads(fj.read_text()) if fj.exists() else {}
    old = rows.get(f"{kind}/{name}", {})
    row.update({"program": f"{kind}/{name}", "stmt_par": stmt_par or bool(old.get("stmt_par_groups")),
                "hand_edits": hand or old.get("hand_edits", []), "legacy": legacy,
                "fix_rounds": old.get("fix_rounds"),
                "stmt_par_groups": groups or old.get("stmt_par_groups", ""),
                "draft_workarounds": pre or old.get("draft_workarounds", [])})
    if old.get("legacy_note"):
        row["legacy_note"] = old["legacy_note"]
    # A legacy build that disagrees while the interpreter agrees is a frozen
    # legacy bug (recorded in notes.md): the entry is recorded on legacy-interp.
    lb = "legacy-interp" if legacy["legacy-build"] != "agrees" and legacy["legacy-interp"] == "agrees" \
        else "legacy-build"
    note = [f"stmt-par groups (source.orig.kara lines): {row['stmt_par_groups']}"] if row["stmt_par_groups"] else []
    if lb == "legacy-interp":
        note.append(f"legacy_backend is legacy-interp: {row.get('legacy_note', 'the legacy build disagrees')} "
                    "(see notes.md); frozen legacy bug")
    meta = read_toml(d / "meta.toml")
    meta.update({"source": f"corpus/apps/{kind}/{name}/source.orig.kara (v2 by karac fix + hand edits)",
                 "dedup_of": [], "expect": "stdout", "exit": 0,
                 "tags": sorted({"apps", f"apps:{kind}"} | ({"stmt-par"} if stmt_par else set())),
                 "class": meta.get("class", "unknown"), "expected_from": "mirror",
                 "legacy_backend": lb, "rules": [], "backends": [], "env": {}, "note": "; ".join(note)})
    sweep(d, name)
    write_toml(d / "meta.toml", meta)
    rows[row["program"]] = {**old, **row}  # keep fields recorded by hand (fix_refused, ...)
    fj.write_text(json.dumps(rows, indent=1, sort_keys=True) + "\n")
    print(json.dumps(row))
    return 0


def table(apps: Path) -> int:
    """Re-measure every app (no legacy runs) and write the friction table into
    corpus/apps/README.md between the TABLE markers."""
    fj = apps / "friction.json"
    rows = json.loads(fj.read_text())
    for prog, row in rows.items():
        m = subprocess.run([sys.executable, __file__, "measure", str(apps / prog)], capture_output=True,
                           text=True, check=True).stdout
        row.update(json.loads(m))
        if not row.get("fix_rounds"):  # rows finalized before fix recorded it: count on a scratch copy
            import tempfile
            with tempfile.TemporaryDirectory() as t:
                tmp = Path(t) / "source.kara"
                shutil.copy(apps / prog / "source.orig.kara", tmp)
                row["fix_rounds"] = fix_to_fixed_point(tmp)
    fj.write_text(json.dumps(rows, indent=1, sort_keys=True) + "\n")

    def kind(h):
        return "derive" if "derive(Copy)" in h else "into_iter" if "into_iter" in h else "other"
    out = ["| program | lines | v2 errors in draft | clones by fix | refs by fix | fix rounds "
           "| hand: derive(Copy) | hand: into_iter | hand: other | shared types | stmt-par "
           "| index-move adapted in draft (legacy refuses) | compiler gaps worked around in draft | legacy vs mirror |",
           "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    tot = collections.Counter()
    for prog in sorted(rows, key=lambda p: (["service", "cli", "pipeline", "graph"].index(p.split("/")[0]), p)):
        r = rows[prog]
        hk = collections.Counter(kind(h) for h in r.get("hand_edits", []))
        would = r.get("fix_would_clone", 0)
        other = hk["other"] - would  # refused-fix clones count under fix, not hand (footnote)
        errs = ", ".join(f"{c} x{n}" for c, n in sorted(r["draft_errors"].items())) or "none"
        fixc = f"{r['clones_by_fix']} (+{would} refused, note 1)" if would else str(r["clones_by_fix"])
        im = any("E_INDEX_MOVE_NON_COPY" in w for w in r.get("draft_workarounds", []))
        lb = r["legacy"]["legacy-build"]
        legacy = "agrees" if lb == "agrees" else "interp agrees; build does not (note 2)"
        # compiler gaps only: a spec-conformant adaptation legacy forces (index-move) or an
        # author's own type error is part of the natural draft, not a gap
        gaps = sum(1 for w in r.get("draft_workarounds", [])
                   if "E_INDEX_MOVE_NON_COPY" not in w and "not a compiler gap" not in w)
        out.append(f"| {prog} | {r['lines']} | {errs} | {fixc} | {r['refs_by_fix']} | {r.get('fix_rounds') or '-'} "
                   f"| {hk['derive']} | {hk['into_iter']} | {other} | {r['shared_types']} "
                   f"| {'yes' if r['stmt_par'] else 'no'} | {'yes' if im else 'no'} | {gaps} | {legacy} |")
        tot.update({"programs": 1, "lines": r["lines"], "errs": sum(r["draft_errors"].values()),
                    "fixc": r["clones_by_fix"] + would, "fixr": r["refs_by_fix"], "derive": hk["derive"],
                    "into_iter": hk["into_iter"], "other": other, "shared": r["shared_types"],
                    "stmtpar": int(r["stmt_par"]), "index_move": int(im), "gaps": gaps, "agree": int(lb == "agrees"),
                    "clean_draft": int(not r["draft_errors"])})
    out.append(f"| **{tot['programs']} programs** | {tot['lines']} | {tot['errs']} | {tot['fixc']} | {tot['fixr']} | "
               f"| {tot['derive']} | {tot['into_iter']} | {tot['other']} | {tot['shared']} | {tot['stmtpar']} "
               f"| {tot['index_move']} | {tot['gaps']} | build agrees on {tot['agree']} |")
    notes = ["", "Per program: hand edits (why) and the compiler gaps the drafting agent worked around.", ""]
    for prog in sorted(rows):
        r = rows[prog]
        notes.append(f"**{prog}**")
        for h in r.get("hand_edits", []):
            notes.append(f"- hand: {h}")
        for h in r.get("draft_workarounds", []):
            notes.append(f"- draft gap: {h}")
        for k in ("fix_refused", "fix_note", "legacy_note"):
            if r.get(k):
                notes.append(f"- {k.replace('_', ' ')}: {r[k]}")
        if r.get("stmt_par_groups"):
            notes.append(f"- stmt-par groups (source.orig.kara lines): {r['stmt_par_groups']}")
        notes.append("")
    readme = apps / "README.md"
    text = readme.read_text()
    a, b = "<!-- TABLE START (scripts/corpus/apps.py table) -->", "<!-- TABLE END -->"
    head, rest = text.split(a)
    _, tail = rest.split(b)
    readme.write_text(head + a + "\n" + "\n".join(out + notes) + "\n" + b + tail)
    print(json.dumps(tot))
    return 0


def main() -> int:
    cmd, d = sys.argv[1], Path(sys.argv[2])
    if cmd == "table":
        return table(d)
    if cmd == "finalize":
        rest = sys.argv[3:]
        opt = lambda flag: [rest[i + 1] for i, a in enumerate(rest) if a == flag]
        groups = opt("--groups")
        return finalize(d.resolve().relative_to(ROOT) if d.is_absolute() else d, "--stmt-par" in rest or bool(groups),
                        opt("--hand"), groups[0] if groups else "", opt("--pre"))
    orig, src = d / "source.orig.kara", d / "source.kara"
    if cmd == "fix":
        shutil.copy(orig, src)
        rounds = fix_to_fixed_point(src)
        shutil.copy(src, d / "source.fixed.kara")  # what `karac fix` alone produced
        fj = d.parent.parent / "friction.json"
        rows = json.loads(fj.read_text()) if fj.exists() else {}
        rows.setdefault(f"{d.parts[-2]}/{d.parts[-1]}", {})["fix_rounds"] = rounds
        fj.write_text(json.dumps(rows, indent=1, sort_keys=True) + "\n")
        left = check(src)
        print(json.dumps({"fix_rounds": rounds, "errors_left": len(left),
                          "left": [f"{e.get('line')}:{e.get('code')} {e.get('message', '')[:90]}" for e in left]},
                         indent=1))
        return 0
    o, f, s = orig.read_text(), (d / "source.fixed.kara").read_text(), src.read_text()
    co, cf, cs = count(o), count(f), count(s)
    hand = [l for l in difflib.unified_diff(f.splitlines(), s.splitlines(), lineterm="", n=0)
            if l[:1] in "+-" and not l.startswith(("+++", "---"))]
    draft_errs = collections.Counter(e.get("code") or e.get("phase") for e in check(orig))
    left = check(src)
    row = {"lines": s.count("\n"), "draft_errors": dict(draft_errs),
           "clones_by_fix": cf["clone"] - co["clone"], "refs_by_fix": cf["ref"] - co["ref"],
           "clones_by_hand": cs["clone"] - cf["clone"], "refs_by_hand": cs["ref"] - cf["ref"],
           "derive_copy_by_hand": cs["derive_copy"] - cf["derive_copy"],
           "into_iter_by_hand": cs["into_iter"] - cf["into_iter"],
           "hand_lines_changed": len(hand), "shared_types": cs["shared"], "check_clean": not left}
    print(json.dumps(row))
    return 0


if __name__ == "__main__":
    sys.exit(main())
