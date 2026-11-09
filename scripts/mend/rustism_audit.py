#!/usr/bin/env python3
"""Inject one Rust-ism at a time into working Kāra programs; see what
`karac check` says and whether `karac fix` restores them.

An LLM that writes Kāra writes Rust where its habits are strongest. This
audit takes programs that check clean and print a known output, makes ONE
such mistake in each, and asks three questions:

  1. Diagnosed: does `karac check` report exactly one new diagnostic for it
     (an error, or a warning for a mistake that is legal but redundant)?
  2. Fixed: does `karac fix` (run until the text stops changing, at most
     three passes) leave a program that checks with no new diagnostic?
  3. Same output: does the fixed program print what the original did, on
     `karac __mir-run`?

A mistake that is valid Kāra (`#[derive(Debug)]`, `Some(ref x)`) passes when
the program checks with nothing new and prints the same.

NOT a Mend rate. The mistakes are injected by a script that knows the
language, not made by a blind model, so this measures the fix surface for
known mistakes, never the machine-fix rate of blind authorship
(examples/mend/harness/mend_batch.py is that).

Usage:
    scripts/mend/rustism_audit.py [--karac PATH] [--programs N] [--only KEY]
                                  [--json OUT] [--markdown OUT] [--keep DIR]
"""
import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent

# ---------------------------------------------------------------- helpers


def in_string_or_comment(line, col):
    """Whether column `col` of `line` is inside a "..." string or after //."""
    quote = False
    i = 0
    while i < col:
        c = line[i]
        if c == "\\" and quote:
            i += 2
            continue
        if c == '"':
            quote = not quote
        elif not quote and line.startswith("//", i):
            return True
        i += 1
    return quote


def first_code_match(src, pattern, flags=0, pred=None):
    """The first match of `pattern` outside strings and comments."""
    for m in re.finditer(pattern, src, flags):
        start = src.rfind("\n", 0, m.start()) + 1
        end = src.find("\n", m.start())
        line = src[start:end if end != -1 else len(src)]
        if in_string_or_comment(line, m.start() - start):
            continue
        if pred and not pred(m, src):
            continue
        return m
    return None


def sub_at(src, m, text, group=0):
    return src[: m.start(group)] + text + src[m.end(group):]


def matching_close(src, open_at):
    """Index of the `}` matching the `{` at `open_at`, skipping strings."""
    depth, i, quote = 0, open_at, False
    while i < len(src):
        c = src[i]
        if quote:
            if c == "\\":
                i += 2
                continue
            if c == '"':
                quote = False
        elif c == '"':
            quote = True
        elif src.startswith("//", i):
            i = src.find("\n", i)
            if i == -1:
                return None
            continue
        elif c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return None


def rust_type(t):
    return t.replace("[", "<").replace("]", ">")


KEYWORDS = {"if", "while", "for", "match", "return", "println", "print", "eprintln",
            "Some", "Ok", "Err", "assert", "assert_eq", "panic", "not", "and", "or",
            "Vec", "Map", "Set", "String", "fn", "let", "in"}

# ---------------------------------------------------------------- injectors
# Each takes the program text and returns it with one mistake, or None when
# the program has no place for it.


def inj_colon_path(src):
    m = first_code_match(src, r"(?<![\w.])([A-Z][A-Za-z0-9]*)\.([A-Z][A-Za-z0-9]*)\b")
    return sub_at(src, m, f"{m.group(1)}::{m.group(2)}") if m else None


def inj_colon_ctor(src):
    m = first_code_match(src, r"\b(String|Vec|Map|Set|VecDeque)\.(new|with_capacity)\(")
    return sub_at(src, m, f"{m.group(1)}::{m.group(2)}(") if m else None


def user_fns(src):
    return set(re.findall(r"^\s*(?:pub\s+)?fn\s+([a-z_]\w*)", src, re.M))


def inj_amp(src):
    fns = user_fns(src) - {"main"}
    m = first_code_match(
        src, r"\b([a-z_]\w*)\(([a-z_]\w*)([,)])",
        pred=lambda m, s: m.group(1) in fns and m.group(2) not in KEYWORDS
        and not re.search(r"\bfn\s+$", s[max(0, m.start() - 10):m.start()]))
    return sub_at(src, m, "&" + m.group(2), 2) if m else None


def inj_amp_mut(src):
    m = first_code_match(
        src, r"[(,]\s*(mut ([a-z_]\w*))(?=\s*[,)])",
        pred=lambda m, s: not re.search(r"(let|for|fn|\|)[^\n;]*$",
                                        s[s.rfind("\n", 0, m.start()) + 1:m.start()]))
    return sub_at(src, m, "&mut " + m.group(2), 1) if m else None


def inj_bang(src):
    m = first_code_match(src, r"\bnot\s+(?=[\w(])")
    return sub_at(src, m, "!") if m else None


PRIMITIVES = {"i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128",
              "isize", "usize", "f32", "f64", "bool", "char", "str"}


def close_bracket(s, open_at):
    """Index of the `]` matching the `[` at `open_at`."""
    depth = 0
    for i in range(open_at, len(s)):
        depth += {"[": 1, "]": -1}.get(s[i], 0)
        if depth == 0:
            return i
    return None


def _vec_literal(m, s):
    """A `Vec[` that opens a list literal, not a type: what follows is an
    element (a number, string, `-`, `(`, or a lowercase name that is no
    primitive type) or the empty list, and the literal is not a receiver."""
    line_start = s.rfind("\n", 0, m.start()) + 1
    head = s[line_start:m.start()]
    # After a `:` with no `=` since, the position is a type annotation.
    if re.search(r":[^=]*$", head):
        return False
    # An enum variant's payload (`    Let(Vec[(String, Expr)], Expr),`).
    if re.match(r"\s*[A-Z]\w*\(", head):
        return False
    rest = s[m.end():]
    head = re.match(r"\s*(\]|[0-9\"'(-]|[a-z_]\w*)", rest)
    if not head or head.group(1) in PRIMITIVES:
        return False
    close = close_bracket(s, m.end() - 1)
    return close is not None and s[close + 1:close + 2] not in (".", "(")


def inj_vec_macro(src):
    m = first_code_match(src, r"(?:(?<=[=(,]\s)|(?<=[=(,])|(?<=return\s))Vec\[",
                         pred=_vec_literal)
    return sub_at(src, m, "vec![") if m else None


def _angle(src, m, at):
    """`src` with the `[..]` opening at `at` written `<..>`."""
    close = close_bracket(src, at)
    if close is None:
        return None
    return src[:at] + "<" + src[at + 1:close] + ">" + src[close + 1:]


def inj_impl_angle(src):
    m = first_code_match(src, r"\bimpl\[")
    return _angle(src, m, m.end() - 1) if m else None


def inj_fn_angle(src):
    m = first_code_match(src, r"\bfn\s+\w+\[")
    return _angle(src, m, m.end() - 1) if m else None


_COLLECT = r"let (mut )?(\w+): ((?:Vec|Set|String|Map)(?:\[[^=\n]*\])?) = ([^;\n]*)\.collect\(\);"


def inj_turbofish(src):
    m = first_code_match(src, _COLLECT)
    if not m:
        return None
    return sub_at(src, m, f"let {m.group(1) or ''}{m.group(2)} = {m.group(4)}"
                          f".collect::<{rust_type(m.group(3))}>();")


def inj_turbofish_hole(src):
    m = first_code_match(src, r"let (mut )?(\w+): (Vec)\[[^=\n]*\] = ([^;\n]*)\.collect\(\);")
    if not m:
        return None
    return sub_at(src, m, f"let {m.group(1) or ''}{m.group(2)} = {m.group(4)}"
                          f".collect::<Vec<_>>();")


def inj_let_mut(src):
    m = first_code_match(src, r"^(\s+)let ([a-z_]\w*)(\s*[:=])", re.M)
    return sub_at(src, m, f"{m.group(1)}let mut {m.group(2)}{m.group(3)}") if m else None


def inj_ref_pattern(src):
    m = first_code_match(src, r"\bSome\(([a-z_]\w*)\)(?=\s*(=>|=[^=>]))")
    return sub_at(src, m, f"Some(ref {m.group(1)})") if m else None


def inj_redundant_as(src):
    m = first_code_match(src, r"\b([a-z_]\w*)\.len\(\)(?![.\w(])")
    return sub_at(src, m, f"{m.group(1)}.len() as i64") if m else None


def _rustify_fstring(body):
    """`a {x} b {y:>4}` -> ("a {} b {:>4}", ["x", "y"]), or None."""
    args, out, i = [], [], 0
    while i < len(body):
        c = body[i]
        if c == "{":
            if body.startswith("{{", i):
                out.append("{{")
                i += 2
                continue
            j = body.find("}", i)
            if j == -1:
                return None
            hole = body[i + 1:j]
            if "{" in hole or '"' in hole:
                return None
            expr, _, spec = hole.partition(":")
            if not expr.strip() or "::" in hole:
                return None
            args.append(expr.strip())
            out.append("{" + (":" + spec if spec else "") + "}")
            i = j + 1
        else:
            out.append(c)
            i += 1
    return "".join(out), args


def inj_println_macro(src):
    for m in re.finditer(r'\bprintln\(f"((?:[^"\\]|\\.)*)"\)', src):
        start = src.rfind("\n", 0, m.start()) + 1
        if in_string_or_comment(src[start:m.start() + 1], m.start() - start):
            continue
        r = _rustify_fstring(m.group(1))
        if not r or not r[1]:
            continue
        fmt, args = r
        return sub_at(src, m, f'println!("{fmt}", {", ".join(args)})')
    return None


def inj_derive_debug(src):
    m = first_code_match(src, r"^(?!#)(struct \w+)", re.M,
                         pred=lambda m, s: not s[:m.start()].rstrip().endswith(")]"))
    return sub_at(src, m, "#[derive(Debug)]\n" + m.group(1)) if m else None


def inj_pub_crate(src):
    m = first_code_match(src, r"^fn (?!main\b)(\w+)", re.M)
    return sub_at(src, m, f"pub(crate) fn {m.group(1)}") if m else None


def inj_wildcard_arm(src):
    enums = set(re.findall(r"^\s*(?:pub\s+)?enum\s+(\w+)", src, re.M))
    for m in re.finditer(r"\bmatch\b[^{\n]*\{", src):
        start = src.rfind("\n", 0, m.start()) + 1
        if in_string_or_comment(src[start:m.start() + 1], m.start() - start):
            continue
        open_at = m.end() - 1
        close = matching_close(src, open_at)
        if close is None:
            continue
        body = src[open_at + 1:close]
        # The arms' patterns: lines at the arm indentation ending a pattern.
        arms = re.findall(r"^\s*([^\n]*?)\s*=>", body, re.M)
        if not arms or any(a.startswith("_") or " if " in a for a in arms):
            continue
        heads = {re.match(r"(\w+)\.", a).group(1) if re.match(r"(\w+)\.", a) else None
                 for a in arms}
        if len(heads) != 1 or None in heads or heads.pop() not in enums:
            continue
        indent = re.search(r"\n(\s*)\S[^\n]*=>", body)
        if not indent:
            continue
        pad = indent.group(1)
        before = src[:close].rstrip()
        sep = "" if before.endswith(",") or before.endswith("}") else ","
        line_start = src.rfind("\n", 0, close) + 1
        close_pad = re.match(r"[ \t]*", src[line_start:]).group(0)
        return before + sep + f"\n{pad}_ => panic(\"unreachable\"),\n" + close_pad + src[close:]
    return None


def inj_question_in_main(src):
    m = re.search(r"^fn main\(\)\s*\{", src, re.M)
    if not m:
        return None
    close = matching_close(src, m.end() - 1)
    if close is None:
        return None
    body = src[m.end():close]
    u = first_code_match(body, r"\.unwrap\(\)")
    if not u:
        return None
    at = m.end() + u.start()
    return src[:at] + "?" + src[at + len(".unwrap()"):]


def inj_tuple_struct(src):
    for m in re.finditer(r"^struct (\w+) \{\s*(\w+): ([^,{}\n]+?),?\s*\}", src, re.M):
        name, field, ty = m.group(1), m.group(2), m.group(3).strip()
        rest = src[m.end():]
        if f"{name} {{" not in rest or not re.search(rf"\.{field}\b", rest):
            continue
        # Another struct with a field of that name would have its `.{field}`
        # renamed too.
        others = src[:m.start()] + src[m.end():]
        if re.search(rf"^\s+(?:pub\s+)?(?:mut\s+)?{field}\s*:", others, re.M):
            continue
        new_rest = re.sub(rf"\b{name} \{{\s*{field}:\s*([^{{}}]*?)\s*\}}", rf"{name}(\1)", rest)
        # A literal the one-line rewrite missed would leave a program no
        # Rust author wrote.
        if re.search(rf"\b{name} \{{\s*{field}\s*[:,}}]", new_rest):
            continue
        new_rest = re.sub(rf"\.{field}\b", ".0", new_rest)
        return src[:m.start()] + f"struct {name}({ty});" + new_rest
    return None


# key, mistake as written, is it legal Kāra, injector
MISTAKES = [
    ("colon_path", "`Enum::Variant` path", False, inj_colon_path),
    ("colon_ctor", "`String::new()` / `Vec::new()`", False, inj_colon_ctor),
    ("amp", "`&x` argument", False, inj_amp),
    ("amp_mut", "`&mut x` argument", False, inj_amp_mut),
    ("bang", "`!cond`", False, inj_bang),
    ("vec_macro", "`vec![..]`", False, inj_vec_macro),
    ("impl_angle", "`impl<T>`", False, inj_impl_angle),
    ("fn_angle", "`fn f<T>`", False, inj_fn_angle),
    ("turbofish", "`.collect::<Vec<T>>()`", False, inj_turbofish),
    ("turbofish_hole", "`.collect::<Vec<_>>()`", False, inj_turbofish_hole),
    ("let_mut", "`let mut` never reassigned", True, inj_let_mut),
    ("ref_pattern", "`Some(ref x)`", True, inj_ref_pattern),
    ("redundant_as", "`x as T` where `x` is a `T`", True, inj_redundant_as),
    ("println_macro", "`println!(\"{}\", x)`", False, inj_println_macro),
    ("derive_debug", "`#[derive(Debug)]`", True, inj_derive_debug),
    ("pub_crate", "`pub(crate) fn`", False, inj_pub_crate),
    ("wildcard_arm", "`_ =>` on an exhaustive enum match", True, inj_wildcard_arm),
    ("question_in_main", "`?` in a `main` returning `()`", False, inj_question_in_main),
    ("tuple_struct", "tuple struct `S(T)` and `.0`", False, inj_tuple_struct),
]
# `while let Some(x) = v.pop()` is valid Kāra with Rust's meaning; it has no
# injection (a loop that pops is not a rewrite of one that iterates), and is
# covered by tests/rustism_valid_forms.rs.

# A mistake that is legal Kāra but redundant is answered by a warning with a
# fix; these are the ones the audit expects one (the rest of the legal ones
# should draw nothing).
WANTS_WARNING = {"let_mut", "redundant_as", "wildcard_arm"}

# ---------------------------------------------------------------- running


def run(karac, args, path, timeout=60):
    """Runs karac in the program's own directory, so a program that writes
    files writes them there."""
    try:
        return subprocess.run([karac, *args, str(path)], capture_output=True, text=True,
                              timeout=timeout, stdin=subprocess.DEVNULL, cwd=path.parent,
                              env={"KARAC_HASH_SEED": "1", "PATH": "/usr/bin:/bin"})
    except subprocess.TimeoutExpired:
        return None


def diagnostics(karac, path):
    p = run(karac, ["check", "--output=json"], path)
    if p is None:
        return None
    try:
        d = json.loads(p.stdout)
    except json.JSONDecodeError:
        return [("error", (p.stderr or p.stdout).strip()[:200], False)]
    out = []
    for x in d.get("diagnostics", []):
        if x.get("primary") is False or x.get("severity") not in ("error", "warning"):
            continue
        fixable = bool(x.get("replacement")) or any(h.get("diff") for h in x.get("hints", []))
        out.append((x["severity"], x["message"], fixable))
    return out


def new_diags(base, now):
    left = Counter((s, m) for s, m, _ in base)
    out = []
    for s, m, f in now:
        if left[(s, m)]:
            left[(s, m)] -= 1
        else:
            out.append((s, m, f))
    return out


def stdout_of(karac, path):
    p = run(karac, ["__mir-run"], path)
    return None if p is None else (p.returncode, p.stdout)


def audit_one(karac, prog, base_diags, base_out, key, legal, inject, work):
    src = prog.read_text(encoding="utf-8")
    bad = inject(src)
    if bad is None or bad == src:
        return None
    f = work / "main.kara"
    f.write_text(bad, encoding="utf-8")
    d = diagnostics(karac, f)
    added = new_diags(base_diags, d or [])
    r = {"program": str(prog.relative_to(ROOT)), "mistake": key,
         "new": [m[:160] for _, m, _ in added],
         "new_fixable": sum(1 for *_, fx in added if fx)}
    for _ in range(3):
        before = f.read_text(encoding="utf-8")
        run(karac, ["fix"], f)
        if f.read_text(encoding="utf-8") == before:
            break
    fixed = f.read_text(encoding="utf-8")
    after = new_diags(base_diags, diagnostics(karac, f) or [])
    out = stdout_of(karac, f) if not any(s == "error" for s, *_ in after) else None
    r["fixed_clean"] = not after
    r["same_output"] = out == base_out
    r["restored"] = fixed == src
    r["left"] = [m[:160] for _, m, _ in after]
    if legal and key not in WANTS_WARNING:
        # Valid Kāra draws nothing; where it draws a diagnostic that is right
        # (moving out of a borrow through `ref`), the fix must restore it.
        ok = r["same_output"] and (not added or r["fixed_clean"])
        r["verdict"] = "pass" if ok else "FAIL"
    else:
        r["diagnosed"] = len(added) == 1
        r["verdict"] = ("pass" if added and r["fixed_clean"] and r["same_output"]
                        else "diagnosed" if added else "SILENT")
    return r


POOLS = ("apps", "katas", "examples", "design", "core")


def eligible():
    """Corpus programs that expect plain stdout, take no input or imports,
    and are small enough to read: (pool, path, the mistakes with a place)."""
    out = []
    for pool in POOLS:
        for p in sorted((ROOT / "corpus" / pool).glob("**/source.kara")):
            meta = p.parent / "meta.toml"
            if not (p.parent / "expected.out").exists() or not meta.exists():
                continue
            m = meta.read_text(encoding="utf-8")
            if 'expect = "stdout"' not in m or "env = {}" not in m:
                continue
            src = p.read_text(encoding="utf-8")
            if "import " in src or len(src) > 12000:
                continue
            sites = {k for k, _, _, inj in MISTAKES if inj(src) not in (None, src)}
            out.append((pool, p, sites))
    return out


def baseline(karac, prog, work):
    """The program's own diagnostics and output, or None when it does not
    check clean and print its expected output here."""
    f = work / "main.kara"
    shutil.copy(prog, f)
    base = diagnostics(karac, f)
    if base is None or any(s == "error" for s, *_ in base):
        return None
    out = stdout_of(karac, f)
    want = (prog.parent / "expected.out").read_text(encoding="utf-8")
    if out is None or out[0] != 0 or out[1] != want:
        return None
    return base, out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--karac", default=str(ROOT / "target" / "release" / "karac"))
    ap.add_argument("--programs", type=int, default=20,
                    help="programs audited for every mistake, half apps, half katas")
    ap.add_argument("--min-per-mistake", type=int, default=5,
                    help="programs added for a mistake the first ones have no place for")
    ap.add_argument("--only")
    ap.add_argument("--json", dest="json_out")
    ap.add_argument("--markdown")
    args = ap.parse_args()
    karac = str(Path(args.karac).resolve())
    mistakes = [m for m in MISTAKES if not args.only or m[0] == args.only]

    chosen, results = [], []
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        progs = eligible()

        def audit(prog, base, out, which):
            for key, _, legal, inject in which:
                r = audit_one(karac, prog, base, out, key, legal, inject, work)
                if r:
                    results.append(r)
                    print(f"{r['verdict']:9} {key:16} {r['program']}"
                          + ("" if r["verdict"] == "pass" else
                             f"  new={r['new'][:2]} left={r['left'][:2]}"), flush=True)

        # The main set: the apps and katas with the most places for a mistake.
        per_pool = (args.programs + 1) // 2
        for pool in ("apps", "katas"):
            ranked = sorted((p for p in progs if p[0] == pool),
                            key=lambda p: (-len(p[2]), str(p[1])))
            taken = 0
            for _, prog, _ in ranked:
                if taken == per_pool:
                    break
                b = baseline(karac, prog, work)
                if b is None:
                    continue
                taken += 1
                chosen.append(str(prog.relative_to(ROOT)))
                audit(prog, *b, mistakes)

        # Then top up each mistake the main set had too few places for.
        for m in mistakes:
            have = sum(1 for r in results if r["mistake"] == m[0])
            for _, prog, sites in progs:
                if have >= args.min_per_mistake:
                    break
                rel = str(prog.relative_to(ROOT))
                if m[0] not in sites or rel in chosen:
                    continue
                b = baseline(karac, prog, work)
                if b is None:
                    continue
                chosen.append(rel)
                before = len(results)
                audit(prog, *b, [m])
                have += len(results) - before

    rows = []
    for key, title, legal, _ in mistakes:
        rs = [r for r in results if r["mistake"] == key]
        if not rs:
            rows.append((title, 0, 0, 0, 0))
            continue
        one = sum(1 for r in rs if len(r["new"]) == 1)
        fixed = sum(1 for r in rs if r["fixed_clean"] and r["same_output"])
        passed = sum(1 for r in rs if r["verdict"] == "pass")
        rows.append((title, len(rs), one, fixed, passed))
    table = ["| Mistake | Programs | One new diagnostic | Fixed, same output | Pass |",
             "|---|---|---|---|---|"]
    for title, n, one, fixed, passed in rows:
        table.append(f"| {title} | {n} | {one}/{n} | {fixed}/{n} | {passed}/{n} |")
    print("\n".join(table))
    print("programs:", len(chosen))
    if args.json_out:
        Path(args.json_out).write_text(json.dumps({"programs": chosen, "results": results},
                                                  indent=1), encoding="utf-8")
    if args.markdown:
        Path(args.markdown).write_text("\n".join(table) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
