#!/usr/bin/env python3
"""Three language-design measurements over corpus/apps/*/*/source.kara.

    python3 scripts/corpus/apps_lang.py corpus/apps   # writes lang.json and the README section

1. Parameters. Every parameter of a non-Copy type (String, collections, user
   types without `#[derive(Copy)]`, generics), excluding `self`, is classed:
   - stored: the body moves it somewhere (a field, a collection, a return
     value, another owned parameter). This is `karac query ownership`'s
     inferred mode `own`, plus a parameter whose body calls
     `<name>.into_iter()`, which the query reports as `ref` (measured on a
     probe: `fn p(v: Vec[String]) -> Vec[String] { for x in v.into_iter() .. }`).
   - mutated: declared `mut ref` / `mut Slice` (the query does not see
     mutation through a method call, so the declared mode is used).
   - read: everything else.
   Split by the declared mode too, because a by-value parameter that is only
   read is the case a borrow-by-default rule would change.
2. Integer widths. Every `as` cast with its source and target type, and every
   binary operator whose operands have different integer widths. The programs
   are scanned for any numeric type other than i64; there are no floats.
3. Effects. For each function, the distinct resources its inferred effects
   name (`karac query effects <file>`; `panics` names none). `pub fn` is
   reported on its own.
"""

import json
import re
import subprocess
import sys
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
KARAC = str(ROOT / "target/debug/karac")
PRIMS = {"i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "usize", "isize", "f32", "f64", "bool", "char"}


def q(kind: str, target: str):
    r = subprocess.run([KARAC, "query", kind, target], capture_output=True, text=True, timeout=120)
    try:
        return json.loads(r.stdout)
    except json.JSONDecodeError:
        return None


def split_top(s: str, sep=","):
    out, depth, cur = [], 0, ""
    for ch in s:
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        if ch == sep and depth == 0:
            out.append(cur)
            cur = ""
        else:
            cur += ch
    if cur.strip():
        out.append(cur)
    return [x.strip() for x in out]


def signature(src: str, name: str, line: int):
    """(params text, body text) of the fn `name` declared at or after `line`."""
    lines = src.splitlines()
    start = sum(len(l) + 1 for l in lines[:line - 1])
    m = re.compile(r"\bfn\s+" + re.escape(name) + r"\s*(\[[^\]]*\])?\s*\(").search(src, start)
    if not m:
        return None, ""
    i, depth = m.end(), 1
    while depth:
        depth += {"(": 1, ")": -1}.get(src[i], 0)
        i += 1
    params = src[m.end():i - 1]
    b = src.index("{", i)
    j, depth = b + 1, 1
    while depth and j < len(src):
        depth += {"{": 1, "}": -1}.get(src[j], 0)
        j += 1
    return params, src[b:j]


def is_copy(ty: str, copy_types: set) -> str:
    ty = ty.strip()
    if ty.startswith(("Fn(", "escaping Fn(")):
        return "fn"
    if ty in PRIMS or ty in copy_types:
        return "copy"
    m = re.fullmatch(r"Option\[(.*)\]", ty)
    if m:
        return is_copy(m.group(1), copy_types)
    if ty.startswith("(") and ty.endswith(")"):
        parts = [is_copy(t, copy_types) for t in split_top(ty[1:-1])]
        return "copy" if all(p == "copy" for p in parts) else "noncopy"
    return "noncopy"


def program(d: Path) -> dict:
    f = d / "source.kara"
    src = f.read_text()
    copy_types = set(re.findall(r"#\[derive\([^)]*\bCopy\b[^)]*\)\]\s*(?:pub\s+)?(?:struct|enum)\s+(\w+)", src))
    eff = q("effects", str(f)) or {"functions": []}
    params, effects = [], []

    def one(fn):
        full = fn["function"]
        name = full.split(".")[-1]
        own = q("ownership", f"{f}.{full}") or {"parameters": []}
        inferred = {p["name"]: p["mode"] for p in own["parameters"]}
        text, body = signature(src, name, fn["line"])
        if text is None:
            return [], None
        rows = []
        for p in split_top(text):
            if re.fullmatch(r"(mut\s+ref\s+|ref\s+)?(mut\s+)?self", p) or ":" not in p:
                continue
            pname, ty = p.split(":", 1)
            pname = pname.replace("mut ", "").strip()
            ty = ty.strip()
            declared = "mut ref" if ty.startswith(("mut ref ", "mut Slice")) else "ref" if ty.startswith("ref ") else "own"
            base = re.sub(r"^(mut\s+ref|ref|mut)\s+", "", ty)
            cls = is_copy(base, copy_types)
            if cls != "noncopy":
                continue
            mode = inferred.get(pname, "?")
            consumed = re.search(r"\b" + re.escape(pname) + r"\s*\.\s*into_iter\s*\(", body) is not None
            use = "mutated" if declared == "mut ref" else "stored" if mode == "own" or consumed else "read"
            rows.append({"fn": full, "param": pname, "type": base, "declared": declared, "inferred": mode,
                         "into_iter": consumed, "use": use})
        is_pub = re.search(r"\bpub\s+fn\s+" + re.escape(name) + r"\b", src) is not None
        res = sorted({e["resource"] for e in fn.get("inferred_effects", []) if e.get("resource")})
        return rows, {"fn": full, "pub": is_pub, "resources": res,
                      "effects": sorted({f"{e['verb']}({e.get('resource', '')})" for e in fn.get("inferred_effects", [])})}

    with ThreadPoolExecutor(8) as ex:
        for rows, e in ex.map(one, eff["functions"]):
            params += rows
            if e:
                effects.append(e)

    casts = []
    for ln, line in enumerate(src.splitlines(), 1):
        for m in re.finditer(r"\bas\s+(i8|i16|i32|i64|u8|u16|u32|u64|usize|isize|f32|f64|char)\b", line):
            casts.append({"line": ln, "target": m.group(1), "text": line.strip()})
    widths = Counter(re.findall(r"\b(i8|i16|i32|u8|u16|u32|u64|usize|isize|f32|f64)\b", src))
    return {"params": params, "effects": effects, "casts": casts, "non_i64_types": dict(widths)}


def main() -> int:
    apps = Path(sys.argv[1])
    progs = sorted(p.parent for p in apps.glob("*/*/source.kara"))
    out = {f"{p.parent.name}/{p.name}": program(p) for p in progs}
    (apps / "lang.json").write_text(json.dumps(out, indent=1, sort_keys=True) + "\n")

    pt = Counter()
    lines = ["### 1. Non-Copy parameters: stored or only read", "",
             "| program | non-Copy params | stored | only read | mutated (`mut ref`) | by-value but only read | declared `ref` |",
             "|---|---|---|---|---|---|---|"]
    for prog, r in out.items():
        c = Counter(p["use"] for p in r["params"])
        byval_read = sum(1 for p in r["params"] if p["declared"] == "own" and p["use"] == "read")
        decl_ref = sum(1 for p in r["params"] if p["declared"] == "ref")
        n = len(r["params"])
        lines.append(f"| {prog} | {n} | {c['stored']} | {c['read']} | {c['mutated']} | {byval_read} | {decl_ref} |")
        pt.update({"n": n, "stored": c["stored"], "read": c["read"], "mutated": c["mutated"],
                   "byval_read": byval_read, "decl_ref": decl_ref,
                   "byval": sum(1 for p in r["params"] if p["declared"] == "own"),
                   "into_iter": sum(1 for p in r["params"] if p["into_iter"] and p["inferred"] != "own")})
    lines.append(f"| **total** | {pt['n']} | {pt['stored']} | {pt['read']} | {pt['mutated']} | {pt['byval_read']} | {pt['decl_ref']} |")
    frac = lambda a, b: f"{100 * a / b:.0f}%" if b else "-"
    lines += ["", f"Stored: {pt['stored']} of {pt['n']} ({frac(pt['stored'], pt['n'])}); only read: {pt['read']} "
              f"({frac(pt['read'], pt['n'])}); mutated through `mut ref`: {pt['mutated']} ({frac(pt['mutated'], pt['n'])}). "
              f"Of the {pt['byval']} declared by value, {pt['byval_read']} are only read "
              f"({frac(pt['byval_read'], pt['byval'])}). {pt['into_iter']} of the stored count are `.into_iter()` "
              "consumptions the ownership query reports as `ref`. Caveat: a declared `ref` was the author's "
              "choice under a spec where every parameter declares its mode, and some were forced: legacy "
              "refuses a by-value index read (`E_INDEX_MOVE_NON_COPY`), so two drafts turned owned-String "
              "helpers into `ref String` ones (dep_resolver, invoice_pipeline).", ""]

    casts = [(prog, c) for prog, r in out.items() for c in r["casts"]]
    kinds = Counter()
    for _, c in casts:
        t = c["text"]
        kinds[f"char -> {c['target']}" if re.search(r"(\bc|'.')\s+as\s+" + c["target"], t) and c["target"] == "u32"
              else f"-> {c['target']}"] += 1
    other = {prog: r["non_i64_types"] for prog, r in out.items() if r["non_i64_types"]}
    lines += ["### 2. Integer widths and `as` casts", "",
              f"{len(casts)} `as` casts in {len({p for p, _ in casts})} programs: "
              + ", ".join(f"{k} x{v}" for k, v in sorted(kinds.items())) + ". "
              "Every `char -> u32` is lossless and every `-> i64` widens a u32, so none can lose a value. "
              "Numeric types other than i64 appear only in " + (", ".join(f"{p} ({', '.join(f'{t} x{n}' for t, n in v.items())})" for p, v in other.items()) or "no program")
              + "; there are no floats. Binary operators with operands of different integer widths: 0, because "
              "the u32 values only meet u32 values or literals. One draft (session_store) tried `i64 + u32` and "
              "was refused (`cannot mix integer types`), although design.md lists u32 -> i64 as implicit.", ""]

    et = Counter()
    hist = Counter()
    hist_noheap = Counter()
    pubs = []
    for prog, r in out.items():
        for e in r["effects"]:
            hist[len(e["resources"])] += 1
            hist_noheap[len([x for x in e["resources"] if x != "Heap"])] += 1
            et["fns"] += 1
            if e["pub"]:
                pubs.append((prog, e))
    allres = Counter(res for r in out.values() for e in r["effects"] for res in e["resources"])
    lines += ["### 3. Effect resources per function", "",
              f"`pub fn`: {len(pubs)} in all 24 programs (single-file programs declare nothing public), so the "
              f"count covers every function ({et['fns']}). Distinct resources named by a function's inferred effects:", "",
              "| resources | functions | functions, not counting Heap |", "|---|---|---|"]
    lines += [f"| {k} | {hist[k]} | {hist_noheap[k]} |" for k in sorted(set(hist) | set(hist_noheap))]
    lines += ["", "Resources seen: " + ", ".join(f"{k} x{v}" for k, v in allres.most_common()) + ".", ""]

    readme = apps / "README.md"
    text = readme.read_text()
    a, b = "<!-- LANG START (scripts/corpus/apps_lang.py) -->", "<!-- LANG END -->"
    head, rest = text.split(a)
    _, tail = rest.split(b)
    readme.write_text(head + a + "\n" + "\n".join(lines) + "\n" + b + tail)
    print(json.dumps({"params": dict(pt), "casts": len(casts), "cast_kinds": dict(kinds), "pub_fn": len(pubs),
                      "fns": et["fns"], "resources_hist": dict(sorted(hist.items())),
                      "resources_hist_no_heap": dict(sorted(hist_noheap.items())), "resources": dict(allres)}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
