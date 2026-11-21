#!/usr/bin/env python3
"""design.md §6 "Operators and precedence", checked exhaustively.

Every ordered pair of the binary integer operators (`|` `^` `&` `<<` `>>`
`+` `-` `*` `/` `%`) is written as `a o1 b o2 c`, with and without a prefix
`-` or `~` on the first operand, and every pair again on both sides of each
comparison, and the logical words over comparisons. Each expression is a
function of its operands, called on several operand tuples.

The expected value comes from a precedence-climbing parser of the page's
table and an evaluator of the page's integer rules (design.md §5 Numeric
semantics: `/` truncates, `%` takes the dividend's sign, a shift amount
outside 0..64 traps; tuples whose evaluation would trap are not called).
The parser is itself checked against Python's: for every expression that
uses no `not`, the page's table and Python's grammar agree level for level,
so `ast.parse` must give the same tree. `not` is where they part (Python's
`not` is looser than comparison, the page's is a prefix operator binding
tighter), so those expressions follow the page alone.

A check only shows the parse when the other grouping gives a different
value on one of the tuples it ran; the script counts the expressions where
that holds. When the generated tuples do not tell the groupings apart, a
search adds one that does; when none exists the two groupings are the same
function (`a + b + c`, `a * b << c`), and the expression is counted apart.

    python3 conformance/precedence.py [--seed N]
"""

import argparse
import ast
import itertools
import os
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import Report, run  # noqa: E402

# The page's table, loosest first; prefix `not - ~` sit above `as`.
LEVEL = {"or": 3, "and": 4, "==": 5, "!=": 5, "<": 5, "<=": 5, ">": 5, ">=": 5,
         "|": 6, "^": 7, "&": 8, "<<": 9, ">>": 9, "+": 10, "-": 10, "*": 11, "/": 11, "%": 11}
INT_OPS = ["|", "^", "&", "<<", ">>", "+", "-", "*", "/", "%"]
CMP_OPS = ["==", "!=", "<", "<=", ">", ">="]
MIN, MAX = -(1 << 63), (1 << 63) - 1


class Trap(Exception):
    pass


def parse(tokens):
    """Precedence climbing over the page's table. Trees are tuples."""
    pos = [0]

    def peek():
        return tokens[pos[0]] if pos[0] < len(tokens) else None

    def unary():
        t = peek()
        if t in ("-", "~", "not"):
            pos[0] += 1
            return ("u" + t, unary())
        if t == "(":
            pos[0] += 1
            e = binary(0)
            assert tokens[pos[0]] == ")"
            pos[0] += 1
            return e
        pos[0] += 1
        return t

    def binary(min_level):
        lhs = unary()
        while peek() in LEVEL and LEVEL[peek()] > min_level:
            op = peek()
            pos[0] += 1
            rhs = binary(LEVEL[op])  # left associative: the right side binds tighter
            lhs = (op, lhs, rhs)
        return lhs

    e = binary(0)
    assert pos[0] == len(tokens), tokens
    return e


PY_BIN = {ast.BitOr: "|", ast.BitXor: "^", ast.BitAnd: "&", ast.LShift: "<<", ast.RShift: ">>",
          ast.Add: "+", ast.Sub: "-", ast.Mult: "*", ast.Div: "/", ast.Mod: "%"}
PY_CMP = {ast.Eq: "==", ast.NotEq: "!=", ast.Lt: "<", ast.LtE: "<=", ast.Gt: ">", ast.GtE: ">="}


def py_tree(n):
    if isinstance(n, ast.Expression):
        return py_tree(n.body)
    if isinstance(n, ast.Name):
        return n.id
    if isinstance(n, ast.BinOp):
        return (PY_BIN[type(n.op)], py_tree(n.left), py_tree(n.right))
    if isinstance(n, ast.UnaryOp):
        return ({ast.USub: "u-", ast.Invert: "u~", ast.Not: "unot"}[type(n.op)], py_tree(n.operand))
    if isinstance(n, ast.Compare):
        assert len(n.ops) == 1
        return (PY_CMP[type(n.ops[0])], py_tree(n.left), py_tree(n.comparators[0]))
    if isinstance(n, ast.BoolOp):
        op = "and" if isinstance(n.op, ast.And) else "or"
        t = py_tree(n.values[0])
        for v in n.values[1:]:
            t = (op, t, py_tree(v))
        return t
    raise ValueError(ast.dump(n))


def chk(v):
    if not MIN <= v <= MAX:
        raise Trap("overflow")
    return v


def ev(t, env):
    if isinstance(t, str):
        return env[t]
    if t[0] == "u-":
        return chk(-ev(t[1], env))
    if t[0] == "u~":
        return ~ev(t[1], env)
    if t[0] == "unot":
        return not ev(t[1], env)
    op, l, r = t
    if op == "and":
        return ev(l, env) and ev(r, env)
    if op == "or":
        return ev(l, env) or ev(r, env)
    x, y = ev(l, env), ev(r, env)
    if op in CMP_OPS:
        return {"==": x == y, "!=": x != y, "<": x < y, "<=": x <= y, ">": x > y, ">=": x >= y}[op]
    if op == "+":
        return chk(x + y)
    if op == "-":
        return chk(x - y)
    if op == "*":
        return chk(x * y)
    if op in ("/", "%"):
        if y == 0:
            raise Trap("division by zero")
        if x == MIN and y == -1:
            raise Trap("overflow")
        q = abs(x) // abs(y) * (1 if (x < 0) == (y < 0) else -1)
        return q if op == "/" else x - q * y
    if op in ("<<", ">>"):
        if not 0 <= y < 64:
            raise Trap("shift amount out of range")
        if op == ">>":
            return x >> y
        v = (x << y) & ((1 << 64) - 1)
        return v - (1 << 64) if v >> 63 else v
    return {"|": x | y, "^": x ^ y, "&": x & y}[op]


def regroup(t):
    """The other grouping of a two-operator chain, for the discrimination count."""
    if isinstance(t, tuple) and len(t) == 3:
        op, l, r = t
        if isinstance(l, tuple) and len(l) == 3 and l[0] not in ("u-", "u~", "unot"):
            return (l[0], l[1], (op, l[2], r))
        if isinstance(r, tuple) and len(r) == 3:
            return (r[0], (op, l, r[1]), r[2])
    return None


def show(v):
    return ("true" if v else "false") if isinstance(v, bool) else str(v)


def exprs():
    """(text, operand names, result type, pair key) for every generated expression."""
    out = []
    for o1, o2 in itertools.product(INT_OPS, INT_OPS):
        for pre in ("", "-", "~"):
            out.append((f"{pre}a {o1} b {o2} c".replace("-a", "- a").replace("~a", "~ a"),
                        "abc", "i64", (o1, o2)))
    for o1, o2 in itertools.product(INT_OPS, INT_OPS):
        for c in CMP_OPS:
            out.append((f"a {o1} b {c} c {o2} d", "abcd", "bool", (o1, c, o2)))
    for c1, c2 in itertools.product(CMP_OPS, CMP_OPS):
        for lg in ("and", "or"):
            out.append((f"a {c1} b {lg} c {c2} d", "abcd", "bool", (c1, lg, c2)))
    for l1, l2 in itertools.product(("and", "or"), repeat=2):
        out.append((f"p {l1} q {l2} r", "pqr", "bool", (l1, l2)))
        out.append((f"not p {l1} q {l2} r", "pqr", "bool", ("not", l1, l2)))
    for c in ("==", "!="):
        out.append((f"not p {c} q", "pq", "bool", ("not", c)))
        out.append((f"p {c} not q", "pq", "bool", (c, "not")))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--tuples", type=int, default=8)
    a = ap.parse_args()
    rng = random.Random(a.seed)
    rep = Report("precedence")

    fns, calls, want, labels = [], [], [], []
    undiscriminated = []
    equivalent = []  # both groupings are the same function (`a + b + c`, `a * b << c`)
    py_checked = 0
    for i, (text, names, ty, key) in enumerate(exprs()):
        tree = parse(text.split())
        if "not" not in text:
            if py_tree(ast.parse(text, mode="eval")) != tree:
                rep.failed.append(f"oracle parser disagrees with Python on {text!r}")
                continue
            py_checked += 1
        params = ", ".join(f"{n}: {'bool' if n in 'pqr' else 'i64'}" for n in names)
        fns.append(f"fn e{i}({params}) -> {ty} {{ {text} }}")
        alt = regroup(tree)
        disc = False
        if names == "pqr" or names == "pq":
            tuples = [dict(zip(names, bits)) for bits in itertools.product((False, True), repeat=len(names))]
        else:
            tuples = []
            for _ in range(a.tuples):
                tuples.append({n: rng.choice([rng.randint(-40, 40), rng.randint(0, 6), rng.randint(-5000, 5000)])
                               for n in names})
        if alt is not None and names not in ("pqr", "pq"):
            # Add a tuple on which the two groupings differ, when one exists.
            for _ in range(4000):
                env = {n: rng.randint(-60, 60) if rng.random() < 0.7 else rng.randint(0, 8) for n in names}
                try:
                    v = ev(tree, env)
                except Trap:
                    continue
                try:
                    if ev(alt, env) != v:
                        tuples.append(env)
                        break
                except Trap:
                    tuples.append(env)
                    break
            else:
                equivalent.append(text)
                alt = None
        for k, env in enumerate(tuples):
            try:
                v = ev(tree, env)
            except Trap:
                continue
            try:
                if alt is not None and ev(alt, env) != v:
                    disc = True
            except Trap:
                disc = True
            args = ", ".join(show(env[n]) for n in names)
            calls.append(f'    println(f"{i} {k} {{e{i}({args})}}");')
            want.append(f"{i} {k} {show(v)}")
            labels.append(f"{text} with {args}")
        if alt is not None and not disc:
            # A bool chain ran on its whole truth table: no tuple left to find.
            (equivalent if names in ("pqr", "pq") else undiscriminated).append(text)

    print(f"{len(fns)} expressions, {len(want)} calls; oracle parser == Python's on {py_checked}; "
          f"{len(equivalent)} whose two groupings are the same function; "
          f"{len(undiscriminated)} not discriminated")
    for t in undiscriminated:
        rep.failed.append(f"no tuple tells the two groupings of {t!r} apart")

    src = "\n".join(fns) + "\n\nfn main() {\n" + "\n".join(calls) + "\n}\n"
    lanes = tuple(os.environ.get("CONF_LANES", "interp,build").split(","))
    res = run(src, backends=lanes, timeout=1800)
    if res["check"][0] != 0:
        rep.failed.append("check refused the program:\n" + (res["check"][1] + res["check"][2])[-3000:])
        rep.finish()
    for backend in lanes:
        for code, out, err in res.get(backend, []):
            if code != 0:
                rep.failed.append(f"[{backend}] exit {code}: {err[-1500:]}")
            rep.compare(backend, labels, out.splitlines(), want)
    rep.finish()


if __name__ == "__main__":
    main()
