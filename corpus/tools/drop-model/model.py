#!/usr/bin/env python3
"""Drop reference model for Kara v2 (review/core-semantics-draft.md, DRAFT 2).

A program is built from the dataclasses below. The model does two independent
things with it:

* `emit(prog)` prints Kara source.
* `run(prog)` executes the program directly against the draft's rules
  (§3 moves, §4 parameters and patterns, §7 destruction, §8 order) with plain
  dynamic state: every holder is a Cell, a move writes MOVED into it, and a
  scope end drops whatever is still there. There are no drop flags and no
  static analysis, so the schedule it prints is the spec's, not an
  implementation's.

It imports nothing from karac. Its limit: it checks moves DYNAMICALLY, so a
use-after-move on a path the run does not take (a maybe-moved place that is in
fact initialized) is not reported. Generators must produce statically valid
programs; the model only guards the paths it executes.

Deliberate reading of one spec gap: an errdefer runs when the function exits
with an Err value by any route (return Err, tail Err). The draft names `?` and
`return Err(...)`; a tail `Err` is the open case (Appendix B).
"""
from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Optional, Union

# ───────────────────────────── program AST ─────────────────────────────


@dataclass
class Lit:
    n: int


@dataclass
class BoolLit:
    b: bool


@dataclass
class StrLit:  # emitted as "s".to_string(); move-only, unobservable drop
    s: str


@dataclass
class Var:
    name: str


@dataclass
class Field:
    base: "Expr"
    name: str


@dataclass
class TupIdx:
    base: "Expr"
    i: int


@dataclass
class New:  # R { id: <id> }
    id: "Expr"


@dataclass
class StructLit:
    ty: str
    fields: list  # [(fname, Expr)] in WRITTEN order


@dataclass
class TupleLit:
    elems: list


@dataclass
class VecLit:
    elems: list


@dataclass
class SomeE:
    e: "Expr"


@dataclass
class NoneE:
    pass


@dataclass
class EnumLit:
    ty: str
    variant: str
    args: list


@dataclass
class OkE:
    e: "Expr"


@dataclass
class ErrE:
    e: "Expr"


@dataclass
class Call:
    fn: str
    args: list


@dataclass
class Add:
    a: "Expr"
    b: "Expr"


Expr = Union[Lit, BoolLit, StrLit, Var, Field, TupIdx, New, StructLit, TupleLit,
             VecLit, SomeE, NoneE, EnumLit, OkE, ErrE, Call, Add]

# patterns


@dataclass
class PBind:
    name: str
    ref: bool = False


@dataclass
class PWild:
    pass


@dataclass
class PSome:
    p: "Pat"


@dataclass
class PNone:
    pass


@dataclass
class PTuple:
    ps: list


@dataclass
class PStruct:
    ty: str
    fields: list  # [(fname, Pat)]
    rest: bool = False


@dataclass
class PEnum:
    ty: str
    variant: str
    ps: list


@dataclass
class POk:
    p: "Pat"


@dataclass
class PErr:
    p: "Pat"


Pat = Union[PBind, PWild, PSome, PNone, PTuple, PStruct, PEnum, POk, PErr]

# statements


@dataclass
class Let:
    pat: Pat
    expr: Expr
    mut: bool = False
    ty: Optional[str] = None


@dataclass
class Assign:
    place: Expr
    expr: Expr


@dataclass
class ExprStmt:
    e: Expr


@dataclass
class Print:
    parts: list  # str | Expr (Expr parts are READ, never moved)


@dataclass
class Block:
    stmts: list


@dataclass
class If:
    cond: Expr
    then: list
    els: Optional[list] = None


@dataclass
class For:
    var: str
    lo: int
    hi: int
    body: list


@dataclass
class Match:
    scrut: Expr
    arms: list  # [(Pat, [stmts])]


@dataclass
class Defer:
    body: list


@dataclass
class Errdefer:
    body: list


@dataclass
class Return:
    e: Optional[Expr] = None


@dataclass
class Break:
    pass


@dataclass
class Continue:
    pass


@dataclass
class Push:
    vec: Expr
    e: Expr


@dataclass
class PanicStmt:  # emitted as an out-of-bounds read
    pass


@dataclass
class StructDef:
    name: str
    fields: list  # [(fname, ty)]
    drop_fmt: Optional[str] = None  # e.g. "d{id}"; fields named in braces are i64


@dataclass
class EnumDef:
    name: str
    variants: list  # [(vname, [ty])]


@dataclass
class Param:
    name: str
    ty: str
    mode: str = "own"  # own | ref


@dataclass
class FnDef:
    name: str
    params: list
    ret: Optional[str]
    body: list
    tail: Optional[Expr] = None


@dataclass
class Program:
    fns: list
    structs: list = field(default_factory=list)
    enums: list = field(default_factory=list)


R_DEF = StructDef("R", [("id", "i64")], "d{id}")

# ───────────────────────────── emitter ─────────────────────────────


def _e(x) -> str:
    if isinstance(x, Lit):
        return str(x.n)
    if isinstance(x, BoolLit):
        return "true" if x.b else "false"
    if isinstance(x, StrLit):
        return f'"{x.s}".to_string()'
    if isinstance(x, Var):
        return x.name
    if isinstance(x, Field):
        return f"{_e(x.base)}.{x.name}"
    if isinstance(x, TupIdx):
        return f"{_e(x.base)}.{x.i}"
    if isinstance(x, New):
        return f"R {{ id: {_e(x.id)} }}"
    if isinstance(x, StructLit):
        return f"{x.ty} {{ " + ", ".join(f"{n}: {_e(v)}" for n, v in x.fields) + " }"
    if isinstance(x, TupleLit):
        return "(" + ", ".join(_e(v) for v in x.elems) + ")"
    if isinstance(x, VecLit):
        return "[" + ", ".join(_e(v) for v in x.elems) + "]"
    if isinstance(x, SomeE):
        return f"Some({_e(x.e)})"
    if isinstance(x, NoneE):
        return "None"
    if isinstance(x, EnumLit):
        if not x.args:
            return f"{x.ty}.{x.variant}"
        return f"{x.ty}.{x.variant}(" + ", ".join(_e(v) for v in x.args) + ")"
    if isinstance(x, OkE):
        return f"Ok({_e(x.e)})"
    if isinstance(x, ErrE):
        return f"Err({_e(x.e)})"
    if isinstance(x, Call):
        return f"{x.fn}(" + ", ".join(_e(v) for v in x.args) + ")"
    if isinstance(x, Add):
        return f"{_e(x.a)} + {_e(x.b)}"
    raise TypeError(x)


def _p(p) -> str:
    if isinstance(p, PBind):
        return ("ref " if p.ref else "") + p.name
    if isinstance(p, PWild):
        return "_"
    if isinstance(p, PSome):
        return f"Some({_p(p.p)})"
    if isinstance(p, PNone):
        return "None"
    if isinstance(p, PTuple):
        return "(" + ", ".join(_p(q) for q in p.ps) + ")"
    if isinstance(p, PStruct):
        parts = [f"{n}: {_p(q)}" for n, q in p.fields] + ([".."] if p.rest else [])
        return f"{p.ty} {{ " + ", ".join(parts) + " }"
    if isinstance(p, PEnum):
        if not p.ps:
            return f"{p.ty}.{p.variant}"
        return f"{p.ty}.{p.variant}(" + ", ".join(_p(q) for q in p.ps) + ")"
    if isinstance(p, POk):
        return f"Ok({_p(p.p)})"
    if isinstance(p, PErr):
        return f"Err({_p(p.p)})"
    raise TypeError(p)


class _Emitter:
    def __init__(self):
        self.lines = []
        self.panics = 0

    def w(self, depth, s):
        self.lines.append("    " * depth + s)

    def block(self, stmts, depth):
        for s in stmts:
            self.stmt(s, depth)

    def stmt(self, s, d):
        if isinstance(s, Let):
            ty = f": {s.ty}" if s.ty else ""
            self.w(d, f"let {'mut ' if s.mut else ''}{_p(s.pat)}{ty} = {_e(s.expr)};")
        elif isinstance(s, Assign):
            self.w(d, f"{_e(s.place)} = {_e(s.expr)};")
        elif isinstance(s, ExprStmt):
            self.w(d, f"{_e(s.e)};")
        elif isinstance(s, Print):
            if all(isinstance(p, str) for p in s.parts):
                self.w(d, 'println("' + "".join(s.parts) + '");')
            else:
                body = "".join(p if isinstance(p, str) else "{" + _e(p) + "}" for p in s.parts)
                self.w(d, 'println(f"' + body + '");')
        elif isinstance(s, Block):
            self.w(d, "{")
            self.block(s.stmts, d + 1)
            self.w(d, "}")
        elif isinstance(s, If):
            self.w(d, f"if {_e(s.cond)} {{")
            self.block(s.then, d + 1)
            if s.els is not None:
                self.w(d, "} else {")
                self.block(s.els, d + 1)
            self.w(d, "}")
        elif isinstance(s, For):
            self.w(d, f"for {s.var} in {s.lo}..{s.hi} {{")
            self.block(s.body, d + 1)
            self.w(d, "}")
        elif isinstance(s, Match):
            self.w(d, f"match {_e(s.scrut)} {{")
            for pat, body in s.arms:
                self.w(d + 1, f"{_p(pat)} => {{")
                self.block(body, d + 2)
                self.w(d + 1, "},")
            self.w(d, "}")
        elif isinstance(s, Defer):
            self.w(d, "defer {")
            self.block(s.body, d + 1)
            self.w(d, "}")
        elif isinstance(s, Errdefer):
            self.w(d, "errdefer {")
            self.block(s.body, d + 1)
            self.w(d, "}")
        elif isinstance(s, Return):
            self.w(d, "return;" if s.e is None else f"return {_e(s.e)};")
        elif isinstance(s, Break):
            self.w(d, "break;")
        elif isinstance(s, Continue):
            self.w(d, "continue;")
        elif isinstance(s, Push):
            self.w(d, f"{_e(s.vec)}.push({_e(s.e)});")
        elif isinstance(s, PanicStmt):
            self.panics += 1
            n = f"oob{self.panics}"
            self.w(d, f"let {n}: Vec[i64] = [1];")
            self.w(d, f'println(f"{{{n}[5]}}");')
        else:
            raise TypeError(s)


def emit(prog: Program) -> str:
    em = _Emitter()
    for sd in [R_DEF] + prog.structs:
        em.w(0, f"struct {sd.name} {{ " + ", ".join(f"{n}: {t}" for n, t in sd.fields) + " }")
        if sd.drop_fmt is not None:
            body = re.sub(r"\{(\w+)\}", r"{self.\1}", sd.drop_fmt)
            em.w(0, f'impl Drop for {sd.name} {{ fn drop(mut ref self) {{ println(f"{body}"); }} }}')
    for ed in prog.enums:
        vs = [v if not tys else f"{v}(" + ", ".join(tys) + ")" for v, tys in ed.variants]
        em.w(0, f"enum {ed.name} {{ " + ", ".join(vs) + " }")
    for fn in prog.fns:
        ps = ", ".join(f"{p.name}: {'ref ' if p.mode == 'ref' else ''}{p.ty}" for p in fn.params)
        ret = f" -> {fn.ret}" if fn.ret else ""
        em.w(0, f"fn {fn.name}({ps}){ret} {{")
        em.block(fn.body, 1)
        if fn.tail is not None:
            em.w(1, _e(fn.tail))
        em.w(0, "}")
    return "\n".join(em.lines) + "\n"

# ───────────────────────────── runtime values ─────────────────────────────


class _Moved:
    def __repr__(self):
        return "MOVED"


MOVED = _Moved()


class ModelError(Exception):
    """The program breaks a v2 rule on the path the model executed."""


class _Panic(Exception):
    pass


class _Ret(Exception):
    def __init__(self, v):
        self.v = v


class _Brk(Exception):
    pass


class _Cont(Exception):
    pass


@dataclass(eq=False)
class IntV:
    n: int


@dataclass(eq=False)
class BoolV:
    b: bool


@dataclass(eq=False)
class StrV:
    s: str


@dataclass(eq=False)
class RecV:
    ty: str
    fields: dict  # declared order


@dataclass(eq=False)
class TupV:
    elems: list


@dataclass(eq=False)
class VecV:
    elems: list


@dataclass(eq=False)
class OptV:
    some: bool
    payload: object = None


@dataclass(eq=False)
class EnumV:
    ty: str
    variant: str
    payload: list


@dataclass(eq=False)
class ResV:
    ok: bool
    payload: object


@dataclass(eq=False)
class RefV:
    cell: "Cell"
    path: list


COPY = (IntV, BoolV)


@dataclass(eq=False)
class Cell:
    v: object
    name: str = ""


class Scope:
    def __init__(self):
        self.entries = []  # ("cell", Cell) | ("defer", stmts) | ("errdefer", stmts)
        self.names = {}

    def bind(self, name, cell):
        cell.name = name
        self.entries.append(("cell", cell))
        self.names[name] = cell


def _contains_moved(v) -> bool:
    if v is MOVED:
        return True
    if isinstance(v, RecV):
        return any(_contains_moved(x) for x in v.fields.values())
    if isinstance(v, (TupV, VecV)):
        return any(_contains_moved(x) for x in v.elems)
    if isinstance(v, EnumV):
        return any(_contains_moved(x) for x in v.payload)
    if isinstance(v, (OptV, ResV)):
        return v.payload is not None and _contains_moved(v.payload)
    return False


def _child(v, st):
    if v is MOVED:
        raise ModelError("use of a moved place")
    k = st[0]
    if k == "f":
        return v.fields[st[1]]
    if k == "i":
        if isinstance(v, (TupV, VecV)):
            return v.elems[st[1]]
        return v.payload[st[1]]
    if k == "p":
        return v.payload
    raise KeyError(st)


def _set_child(v, st, nv):
    k = st[0]
    if k == "f":
        v.fields[st[1]] = nv
    elif k == "i":
        (v.elems if isinstance(v, (TupV, VecV)) else v.payload)[st[1]] = nv
    else:
        v.payload = nv


class Model:
    def __init__(self, prog: Program):
        self.structs = {s.name: s for s in [R_DEF] + prog.structs}
        self.fns = {f.name: f for f in prog.fns}
        self.out: list = []
        self.frames: list = []  # each frame: list[Scope]
        self.temps: list = []  # stack of list[Cell]
        self.depth = 0

    # ── places ──
    def lookup(self, name) -> Cell:
        for sc in reversed(self.frames[-1]):
            if name in sc.names:
                return sc.names[name]
        raise ModelError(f"unbound {name}")

    def resolve(self, e):
        """(cell, path, borrowed) for a place expression, else None."""
        if isinstance(e, Var):
            c = self.lookup(e.name)
            if isinstance(c.v, RefV):
                return (c.v.cell, list(c.v.path), True)
            return (c, [], False)
        if isinstance(e, (Field, TupIdx)):
            b = self.resolve(e.base)
            if b is None:
                return None
            st = ("f", e.name) if isinstance(e, Field) else ("i", e.i)
            return (b[0], b[1] + [st], b[2])
        return None

    def place_of(self, e):
        """A place for e; a non-place expression becomes a temporary of the current statement."""
        pl = self.resolve(e)
        if pl is not None:
            return pl
        if isinstance(e, (Field, TupIdx)):
            c, path, b = self.place_of(e.base)
            st = ("f", e.name) if isinstance(e, Field) else ("i", e.i)
            return (c, path + [st], b)
        c = Cell(self.eval_value(e), "<temp>")
        self.temps[-1].append(c)
        return (c, [], False)

    def get(self, cell, path):
        v = cell.v
        for st in path:
            v = _child(v, st)
        if v is MOVED:
            raise ModelError(f"use of moved place {cell.name}{path}")
        return v

    def set(self, cell, path, nv):
        if not path:
            cell.v = nv
            return
        v = cell.v
        for st in path[:-1]:
            v = _child(v, st)
        if v is MOVED:
            raise ModelError("assignment into a moved value")
        _set_child(v, path[-1], nv)

    def raw(self, cell, path):
        v = cell.v
        for st in path:
            if v is MOVED:
                raise ModelError("assignment into a moved value")
            v = _child(v, st)
        return v

    def move_out(self, cell, path, borrowed):
        v = self.get(cell, path)
        if isinstance(v, COPY):
            return v
        if isinstance(v, RefV):
            return v  # a ref binding copies the reference
        if borrowed:
            raise ModelError("move out of a borrowed place (C3)")
        if _contains_moved(v):
            raise ModelError("use of a partially moved value (C4)")
        anc = cell.v
        for st in path:
            if isinstance(anc, RecV) and self.structs[anc.ty].drop_fmt is not None:
                raise ModelError(f"move out of {anc.ty}, which has a Drop body (C3/C4)")
            if isinstance(anc, VecV):
                raise ModelError("move out of an index (C3)")
            anc = _child(anc, st)
        self.set(cell, path, MOVED)
        return v

    def borrow(self, e):
        c, path, _ = self.place_of(e)
        v = self.get(c, path)
        if _contains_moved(v):
            raise ModelError("borrow of a partially moved value")
        if isinstance(v, RefV):
            return v
        return RefV(c, path)

    def read(self, e):
        c, path, _ = self.place_of(e)
        v = self.get(c, path)
        while isinstance(v, RefV):
            v = self.get(v.cell, v.path)
        return v

    # ── drop ──
    def drop(self, v):
        if v is MOVED or v is None or isinstance(v, (IntV, BoolV, StrV, RefV)):
            return
        if isinstance(v, RecV):
            sd = self.structs[v.ty]
            if sd.drop_fmt is not None:
                if _contains_moved(v):
                    raise ModelError("Drop body over a partially moved value")
                ints = {k: x.n for k, x in v.fields.items() if isinstance(x, IntV)}
                self.out.append(sd.drop_fmt.format(**ints))
            for fname, _ in reversed(sd.fields):
                self.drop(v.fields[fname])
        elif isinstance(v, TupV):
            for x in reversed(v.elems):
                self.drop(x)
        elif isinstance(v, EnumV):
            for x in reversed(v.payload):
                self.drop(x)
        elif isinstance(v, VecV):
            for x in v.elems:
                self.drop(x)
        elif isinstance(v, (OptV, ResV)):
            self.drop(v.payload)
        else:
            raise TypeError(v)

    def drop_temps(self):
        frame = self.temps.pop()
        for c in reversed(frame):
            v, c.v = c.v, MOVED
            self.drop(v)

    def exit_scope(self, sc, err):
        while sc.entries:
            kind, x = sc.entries.pop()
            if kind == "cell":
                v, x.v = x.v, MOVED
                self.drop(v)
            elif kind == "defer" or (kind == "errdefer" and err):
                self.exec_block(x)

    # ── evaluation (left to right, §8) ──
    def eval_value(self, e):
        if isinstance(e, Lit):
            return IntV(e.n)
        if isinstance(e, BoolLit):
            return BoolV(e.b)
        if isinstance(e, StrLit):
            return StrV(e.s)
        if isinstance(e, (Var, Field, TupIdx)):
            return self.move_out(*self.place_of(e))
        if isinstance(e, New):
            return RecV("R", {"id": self.eval_value(e.id)})
        if isinstance(e, StructLit):
            got = {n: self.eval_value(x) for n, x in e.fields}  # written order
            sd = self.structs[e.ty]
            return RecV(e.ty, {n: got[n] for n, _ in sd.fields})
        if isinstance(e, TupleLit):
            return TupV([self.eval_value(x) for x in e.elems])
        if isinstance(e, VecLit):
            return VecV([self.eval_value(x) for x in e.elems])
        if isinstance(e, SomeE):
            return OptV(True, self.eval_value(e.e))
        if isinstance(e, NoneE):
            return OptV(False, None)
        if isinstance(e, EnumLit):
            return EnumV(e.ty, e.variant, [self.eval_value(x) for x in e.args])
        if isinstance(e, OkE):
            return ResV(True, self.eval_value(e.e))
        if isinstance(e, ErrE):
            return ResV(False, self.eval_value(e.e))
        if isinstance(e, Add):
            a = self.eval_value(e.a)
            b = self.eval_value(e.b)
            return IntV(a.n + b.n)
        if isinstance(e, Call):
            fn = self.fns[e.fn]
            if len(fn.params) != len(e.args):
                raise ModelError(f"arity {e.fn}")
            args = []
            for p, a in zip(fn.params, e.args):
                args.append(self.borrow(a) if p.mode == "ref" else self.eval_value(a))
            v = self.invoke(fn, args)
            return v if v is not None else IntV(0)
        raise TypeError(e)

    def invoke(self, fn, args):
        self.depth += 1
        if self.depth > 100:
            raise ModelError("recursion too deep")
        sc = Scope()
        self.frames.append([sc])
        for p, a in zip(fn.params, args):
            sc.bind(p.name, Cell(a))  # §4.2: params are the callee's, introduced first
        try:
            for s in fn.body:
                self.exec_stmt(s)
            v = None
            if fn.tail is not None:
                self.temps.append([])
                v = self.eval_value(fn.tail)
                self.drop_temps()  # §7.4: tail temps before the locals
        except _Ret as r:
            v = r.v
        self.exit_scope(sc, err=isinstance(v, ResV) and not v.ok)
        self.frames.pop()
        self.depth -= 1
        return v

    # ── statements ──
    def exec_block(self, stmts, pre=None):
        sc = Scope()
        self.frames[-1].append(sc)
        try:
            if pre:
                for name, v in pre:
                    sc.bind(name, Cell(v))
            for s in stmts:
                self.exec_stmt(s)
        except _Ret as r:
            self.exit_scope(sc, err=isinstance(r.v, ResV) and not r.v.ok)
            self.frames[-1].pop()
            raise
        except (_Brk, _Cont):
            self.exit_scope(sc, err=False)
            self.frames[-1].pop()
            raise
        self.exit_scope(sc, err=False)
        self.frames[-1].pop()

    def exec_stmt(self, s):
        self.temps.append([])
        try:
            self._exec(s)
        except (_Ret, _Brk, _Cont):
            self.drop_temps()
            raise
        self.drop_temps()

    def _exec(self, s):
        sc = self.frames[-1][-1]
        if isinstance(s, Let):
            pl = self.place_of(s.expr)
            if not self.matches(s.pat, self.get(pl[0], pl[1])):
                raise ModelError("refutable let")
            for name, v in self.bind(s.pat, *pl):
                sc.bind(name, Cell(v))
        elif isinstance(s, Assign):
            pl = self.resolve(s.place)
            if pl is None or pl[2]:
                raise ModelError("assignment to a non-place or through a ref")
            nv = self.eval_value(s.expr)  # §8.2: place operands first (none here), then v
            old = self.raw(pl[0], pl[1])
            self.drop(old)  # §7.5: old value (or its remaining parts) dropped, then store
            self.set(pl[0], pl[1], nv)
        elif isinstance(s, ExprStmt):
            v = self.eval_value(s.e)
            self.temps[-1].append(Cell(v, "<result>"))
        elif isinstance(s, Print):
            buf = []
            for p in s.parts:
                if isinstance(p, str):
                    buf.append(p)
                else:
                    v = self.read(p)
                    buf.append(str(v.n) if isinstance(v, IntV) else v.s if isinstance(v, StrV)
                               else ("true" if v.b else "false"))
            self.out.append("".join(buf))
        elif isinstance(s, Block):
            self.exec_block(s.stmts)
        elif isinstance(s, If):
            self.temps.append([])
            c = self.eval_value(s.cond)
            self.drop_temps()  # §7.4: condition temps before the branch
            if c.b:
                self.exec_block(s.then)
            elif s.els is not None:
                self.exec_block(s.els)
        elif isinstance(s, For):
            for i in range(s.lo, s.hi):
                try:
                    self.exec_block(s.body, pre=[(s.var, IntV(i))])
                except _Cont:
                    continue
                except _Brk:
                    break
        elif isinstance(s, Match):
            pl = self.place_of(s.scrut)  # a temp scrutinee lives to the end of this statement
            v = self.get(pl[0], pl[1])
            for pat, body in s.arms:
                if self.matches(pat, v):
                    binds = self.bind(pat, *pl)
                    self.exec_block(body, pre=binds)
                    break
            else:
                raise ModelError("non-exhaustive match")
        elif isinstance(s, Defer):
            sc.entries.append(("defer", s.body))
        elif isinstance(s, Errdefer):
            sc.entries.append(("errdefer", s.body))
        elif isinstance(s, Return):
            v = self.eval_value(s.e) if s.e is not None else None
            raise _Ret(v)
        elif isinstance(s, Break):
            raise _Brk()
        elif isinstance(s, Continue):
            raise _Cont()
        elif isinstance(s, Push):
            c, path, _ = self.place_of(s.vec)
            vec = self.get(c, path)
            while isinstance(vec, RefV):
                vec = self.get(vec.cell, vec.path)
            vec.elems.append(self.eval_value(s.e))
        elif isinstance(s, PanicStmt):
            raise _Panic()
        else:
            raise TypeError(s)

    # ── patterns (§4.6) ──
    def matches(self, p, v) -> bool:
        while isinstance(v, RefV):
            v = self.get(v.cell, v.path)
        if isinstance(p, (PBind, PWild)):
            return True
        if isinstance(p, PSome):
            return isinstance(v, OptV) and v.some and self.matches(p.p, v.payload)
        if isinstance(p, PNone):
            return isinstance(v, OptV) and not v.some
        if isinstance(p, PTuple):
            return all(self.matches(q, x) for q, x in zip(p.ps, v.elems))
        if isinstance(p, PStruct):
            return all(self.matches(q, v.fields[n]) for n, q in p.fields)
        if isinstance(p, PEnum):
            return v.variant == p.variant and all(self.matches(q, x) for q, x in zip(p.ps, v.payload))
        if isinstance(p, POk):
            return v.ok and self.matches(p.p, v.payload)
        if isinstance(p, PErr):
            return (not v.ok) and self.matches(p.p, v.payload)
        raise TypeError(p)

    def bind(self, p, cell, path, borrowed):
        """Bindings (name, value) left to right, performing the moves."""
        v = self.get(cell, path)
        if isinstance(v, RefV):  # matching through a ref: every binding is a ref (§4.6)
            return self.bind(p, v.cell, v.path, True)
        if isinstance(p, PWild):
            return []
        if isinstance(p, PBind):
            if isinstance(v, COPY):
                return [(p.name, v)]
            if p.ref or borrowed:
                if _contains_moved(v):
                    raise ModelError("borrow of a partially moved value")
                return [(p.name, RefV(cell, path))]
            return [(p.name, self.move_out(cell, path, False))]
        out = []
        if isinstance(p, (PSome, POk, PErr)):
            out += self.bind(p.p, cell, path + [("p",)], borrowed)
        elif isinstance(p, PNone):
            pass
        elif isinstance(p, PTuple):
            for i, q in enumerate(p.ps):
                out += self.bind(q, cell, path + [("i", i)], borrowed)
        elif isinstance(p, PStruct):
            for n, q in p.fields:
                out += self.bind(q, cell, path + [("f", n)], borrowed)
        elif isinstance(p, PEnum):
            for i, q in enumerate(p.ps):
                out += self.bind(q, cell, path + [("i", i)], borrowed)
        else:
            raise TypeError(p)
        return out


def run(prog: Program):
    """(stdout lines, exit code). Raises ModelError for a program v2 rejects."""
    m = Model(prog)
    try:
        m.invoke(m.fns["main"], [])
    except _Panic:
        return m.out, 101
    return m.out, 0
