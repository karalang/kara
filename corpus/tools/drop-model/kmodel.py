"""The drop reference model, run on Kāra source.

model.py runs programs the model builds itself. This runs ordinary Kāra
source (parsed by kparse.py) under the rules of docs/core-semantics.md, so
corpus programs can be checked against the spec without the compiler.

It is a dynamic executor, like model.py: each binding, field and temporary is
a holder; a move marks the holder MOVED; scope exit drops what is left, in
the order §7 gives. A move or borrow rule broken on the executed path raises
ModelError, which means "v2 rejects this program". Anything outside the
subset raises Unsupported, which means "no verdict".

    run_source(src) -> (stdout: str, exit_code: int, flags: set)

flags holds "unordered" when the program dropped elements of a Map or Set
whose drops are observable: §7.8 leaves that order unspecified, so only the
multiset of lines is a valid comparison.
"""
from __future__ import annotations

import re
import sys
from dataclasses import dataclass, field
from typing import Optional

from kparse import Block, FnDef, Program, Unsupported, parse

sys.setrecursionlimit(20000)


class ModelError(Exception):
    """The program breaks a v2 rule on the executed path."""


class KPanic(Exception):
    pass


class Ret(Exception):
    def __init__(self, v, is_err):
        self.v = v
        self.is_err = is_err


class Brk(Exception):
    def __init__(self, v):
        self.v = v


class Cont(Exception):
    pass


class _Moved:
    def __repr__(self):
        return "MOVED"


MOVED = _Moved()

# ───────────────────────────── values ─────────────────────────────


@dataclass(eq=False)
class Prim:  # int, float, bool, char, unit: Copy
    v: object
    kind: str  # int | float | bool | char | unit


UNIT = Prim(None, "unit")


@dataclass(eq=False)
class Str:  # String: move-only, its drop is not observable
    s: str


@dataclass(eq=False)
class Rec:
    ty: str
    fields: dict  # declaration order


@dataclass(eq=False)
class Enum:
    ty: str
    var: str
    payload: object  # list (tuple variant) | dict (struct variant) | None


@dataclass(eq=False)
class Tup:
    elems: list


@dataclass(eq=False)
class VecV:
    elems: list
    kind: str = "Vec"  # Vec | Array


@dataclass(eq=False)
class MapV:
    keys: list
    vals: list
    kind: str = "Map"  # Map | Set | SortedMap | SortedSet


@dataclass(eq=False)
class Cell:
    v: object
    name: str = ""
    temp: bool = False
    refbind: bool = False  # the binding is a reference (a ref parameter or a ref pattern binding)
    count: int = 0  # for a shared box
    loop_depth: int = -1  # loops enclosing the binding's declaration (-1: not a named binding)
    ty: object = None  # declared type (a `let` annotation or a parameter type), when there is one


@dataclass(eq=False)
class Ref:
    cell: Cell
    path: list
    mut: bool = False
    origins: list = field(default_factory=list)  # cells the signature rule (§5.4) says it borrows


@dataclass(eq=False)
class Handle:
    box: Cell  # box.v is the shared object, box.count its reference count


def some(v):
    return Enum("Option", "Some", [v])


def none():
    return Enum("Option", "None", None)


def ok(v):
    return Enum("Result", "Ok", [v])


def err(v):
    return Enum("Result", "Err", [v])


def is_err_result(v) -> bool:
    return isinstance(v, Enum) and v.ty == "Result" and v.var == "Err"


class Scope:
    def __init__(self):
        self.entries = []
        self.names = {}

    depth_of = None  # set by the model: () -> current loop depth

    def bind(self, name, cell):
        cell.name = name
        if Scope.depth_of is not None and not cell.temp:
            cell.loop_depth = Scope.depth_of()
        self.entries.append(("cell", cell))
        self.names[name] = cell


@dataclass
class Frame:
    scopes: list
    self_type: Optional[str] = None
    ret_ref: bool = False
    ret: object = None
    own_ret: bool = False  # an owned, non-generic return type: a reference leaving the body is read/counted/C3
    loops: list = field(default_factory=list)  # per enclosing loop: cells moved in it that were declared outside


def _child(v, st):
    if v is MOVED:
        raise ModelError("use of a moved place")
    k, key = st
    if k == "k":
        return v.keys[key]
    if k == "f":
        if isinstance(v, Rec):
            return v.fields[key]
        if isinstance(v, Enum) and isinstance(v.payload, dict):
            return v.payload[key]
        raise ModelError(f"no field {key}")
    if isinstance(v, (Tup, VecV)):
        if not 0 <= key < len(v.elems):
            raise KPanic("index out of bounds")
        return v.elems[key]
    if isinstance(v, Enum):
        return v.payload[key]
    if isinstance(v, Rec):
        return v.fields[str(key)]
    if isinstance(v, MapV):
        return v.vals[key]
    raise ModelError(f"cannot project {st} of {type(v).__name__}")


def _set_child(v, st, nv):
    k, key = st
    if k == "k":
        v.keys[key] = nv
    elif k == "f":
        if isinstance(v, Rec):
            v.fields[key] = nv
        else:
            v.payload[key] = nv
    elif isinstance(v, (Tup, VecV)):
        v.elems[key] = nv
    elif isinstance(v, Enum):
        v.payload[key] = nv
    elif isinstance(v, Rec):
        v.fields[str(key)] = nv
    elif isinstance(v, MapV):
        v.vals[key] = nv


def _has_moved(v) -> bool:
    if v is MOVED:
        return True
    if isinstance(v, Rec):
        return any(_has_moved(x) for x in v.fields.values())
    if isinstance(v, (Tup, VecV)):
        return any(_has_moved(x) for x in v.elems)
    if isinstance(v, Enum):
        if isinstance(v.payload, dict):
            return any(_has_moved(x) for x in v.payload.values())
        return v.payload is not None and any(_has_moved(x) for x in v.payload)
    return False


# Open question to Gowtham (2026-10-06): does using an Option/tuple made only of shared handles and
# Copy parts count like a bare handle (on), or move as §6.1 says today (off)? KARA_MODEL_COUNT_AGG=0 for the latter.
import os as _os
COUNT_HANDLE_AGGREGATES = _os.environ.get("KARA_MODEL_COUNT_AGG", "1") != "0"

INT_TYPES = {"i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize"}

# ───────────────────────────── the model ─────────────────────────────


class Model:
    def __init__(self, prog: Program):
        self.p = prog
        self.out: list = []
        self.frames: list = []
        self.temps: list = [[]]
        self.globals = Scope()
        self.flags: set = set()
        self.steps = 0
        Scope.depth_of = lambda: len(self.frames[-1].loops) if self.frames else 0

    # ── type facts ──
    def is_copy(self, v) -> bool:
        if isinstance(v, (Prim, Ref)):
            return True
        if isinstance(v, Tup):
            return all(self.is_copy(x) for x in v.elems)
        if isinstance(v, Enum):
            if v.ty in ("Option", "Result"):
                pl = v.payload or []
                return all(self.is_copy(x) for x in pl)
            ed = self.p.enums.get(v.ty)
            return bool(ed and "Copy" in ed.derives)
        if isinstance(v, Rec):
            sd = self.p.structs.get(v.ty)
            return bool(sd and "Copy" in sd.derives)
        if isinstance(v, VecV) and v.kind == "Array":
            return all(self.is_copy(x) for x in v.elems)  # Array[T, N] is Copy when T is (§1.1)
        return False

    def handle_like(self, v) -> bool:
        """Only shared handles and Copy parts, with at least one handle: Option[Node], (Node, i64)."""
        def walk(x):
            if isinstance(x, Handle):
                return True
            if isinstance(x, Enum) and x.ty == "Option" and isinstance(x.payload, list):
                return all(walk(y) or self.is_copy(y) for y in x.payload) and any(walk(y) for y in x.payload)
            if isinstance(x, Tup):
                return all(walk(y) or self.is_copy(y) for y in x.elems) and any(walk(y) for y in x.elems)
            return False
        return walk(v)

    def read_through_ref(self, target):
        """An owned value read through a reference (§6.1): Copy is copied, a handle or handle aggregate
        is counted, anything else returns None (the caller reports C3)."""
        if isinstance(target, Handle):
            target.box.count += 1
            return Handle(target.box)
        if self.is_copy(target):
            return self.copy_value(target)
        if COUNT_HANDLE_AGGREGATES and self.handle_like(target):
            return self.clone_value(target)
        return None

    def declared_type(self, c, p):
        """The declared type of a place: a binding's annotation, or a struct field's declared type."""
        if not p:
            return c.ty
        if p[-1][0] != "f":
            return None
        try:
            parent = self.deref_value(self.get(c, p[:-1]))
        except ModelError:
            return None
        sd = self.p.structs.get(parent.ty) if isinstance(parent, Rec) else None
        return dict(sd.fields).get(p[-1][1]) if sd else None

    def mark_array(self, v, ty):
        """A list value declared `Array[T, N]` is an array (Copy when T is); literals are built as lists."""
        if isinstance(v, VecV) and ty is not None and ty[0] == "app" and ty[1] == "Array":
            v.kind = "Array"

    def elem_types(self, kind):
        """The declared element type(s) of the receiver collection, if known: [T] for Vec/Set, [K, V] for Map."""
        t = getattr(self, "_recv_ty", None)
        if t and t[0] in ("ref", "mutref"):
            t = t[1]
        if t and t[0] == "app" and t[1] == kind and t[2]:
            return t[2]
        return None

    def is_shared_type(self, ty) -> bool:
        sd = self.p.structs.get(ty)
        if sd is not None:
            return sd.shared
        ed = self.p.enums.get(ty)
        return bool(ed and ed.shared)

    def has_drop(self, v) -> bool:
        return isinstance(v, (Rec, Enum)) and v.ty in self.p.drops

    def copy_value(self, v):
        if isinstance(v, (Prim, Ref)):
            return v
        if isinstance(v, Tup):
            return Tup([self.copy_value(x) for x in v.elems])
        if isinstance(v, Enum):
            if isinstance(v.payload, dict):
                return Enum(v.ty, v.var, {k: self.copy_value(x) for k, x in v.payload.items()})
            return Enum(v.ty, v.var, None if v.payload is None else [self.copy_value(x) for x in v.payload])
        if isinstance(v, Rec):
            return Rec(v.ty, {k: self.copy_value(x) for k, x in v.fields.items()})
        if isinstance(v, VecV) and v.kind == "Array":
            return VecV([self.copy_value(x) for x in v.elems], "Array")
        raise ModelError("copy of a move-only value")

    def clone_value(self, v):
        if v is MOVED:
            raise ModelError("clone of a moved value")
        if isinstance(v, Ref):
            return self.clone_value(self.deref_value(v))
        if isinstance(v, Prim):
            return v
        if isinstance(v, Str):
            return Str(v.s)
        if isinstance(v, Handle):
            v.box.count += 1
            return Handle(v.box)
        if isinstance(v, Tup):
            return Tup([self.clone_value(x) for x in v.elems])
        if isinstance(v, VecV):
            return VecV([self.clone_value(x) for x in v.elems], v.kind)
        if isinstance(v, MapV):
            return MapV([self.clone_value(x) for x in v.keys], [self.clone_value(x) for x in v.vals], v.kind)
        if isinstance(v, Enum):
            if isinstance(v.payload, dict):
                return Enum(v.ty, v.var, {k: self.clone_value(x) for k, x in v.payload.items()})
            return Enum(v.ty, v.var, None if v.payload is None else [self.clone_value(x) for x in v.payload])
        if isinstance(v, Rec):
            return Rec(v.ty, {k: self.clone_value(x) for k, x in v.fields.items()})
        raise Unsupported(f"clone of {type(v).__name__}")

    # ── drops (§7.8) ──
    def drop_value(self, v):
        if v is MOVED or isinstance(v, (Prim, Str, Ref)) or v is None:
            return
        if isinstance(v, Handle):
            v.box.count -= 1
            if v.box.count == 0:
                inner = v.box.v
                v.box.v = MOVED
                self.drop_value(inner)
            return
        if isinstance(v, Rec):
            if v.ty in self.p.drops:
                if _has_moved(v):
                    raise ModelError("partially moved value of a type with a Drop body")
                self.run_drop_body(v)
            for k in reversed(list(v.fields)):
                x = v.fields[k]
                v.fields[k] = MOVED
                self.drop_value(x)
            return
        if isinstance(v, Enum):
            if v.ty in self.p.drops:
                self.run_drop_body(v)
            if isinstance(v.payload, dict):
                for k in reversed(list(v.payload)):
                    x = v.payload[k]
                    v.payload[k] = MOVED
                    self.drop_value(x)
            elif v.payload:
                for i in reversed(range(len(v.payload))):
                    x = v.payload[i]
                    v.payload[i] = MOVED
                    self.drop_value(x)
            return
        if isinstance(v, Tup):
            for i in reversed(range(len(v.elems))):
                x = v.elems[i]
                v.elems[i] = MOVED
                self.drop_value(x)
            return
        if isinstance(v, VecV):
            for i in range(len(v.elems)):
                x = v.elems[i]
                v.elems[i] = MOVED
                self.drop_value(x)
            return
        if isinstance(v, MapV):
            idx = list(range(len(v.keys)))
            if v.kind in ("SortedMap", "SortedSet"):
                idx.sort(key=lambda i: self.sort_key(v.keys[i]))
            before = len(self.out)
            for i in idx:
                k = v.keys[i]
                v.keys[i] = MOVED
                self.drop_value(k)
                if v.kind in ("Map", "SortedMap"):
                    x = v.vals[i]
                    v.vals[i] = MOVED
                    self.drop_value(x)
            if v.kind in ("Map", "Set") and len(self.out) > before and len(idx) > 1:
                self.flags.add("unordered")
            return
        raise Unsupported(f"drop of {type(v).__name__}")

    def run_drop_body(self, v):
        fdef = self.p.methods[v.ty]["__drop__"]
        holder = Cell(v, "<dropping>")
        self.call_user(fdef, Ref(holder, [], True), [], v.ty)

    def drop_temps(self, ts):
        for c in reversed(ts):
            x = c.v
            c.v = MOVED
            self.drop_value(x)

    def exit_scope(self, sc: Scope, is_err: bool):
        for kind, x in reversed(sc.entries):
            if kind == "cell":
                if x.refbind:
                    continue
                v = x.v
                x.v = MOVED
                self.drop_value(v)
            elif kind == "defer" or (kind == "errdefer" and is_err):
                self.eval_block(x)
        sc.entries = []

    # ── places ──
    @property
    def frame(self) -> Frame:
        return self.frames[-1]

    def lookup(self, name) -> Cell:
        for sc in reversed(self.frame.scopes):
            c = sc.names.get(name)
            if c is not None:
                return c
        c = self.globals.names.get(name)
        if c is not None:
            return c
        if name in self.p.consts:
            self.temps.append([])
            v = self.value(self.p.consts[name])
            self.temps.pop()
            c = Cell(v)
            self.globals.bind(name, c)
            return c
        raise Unsupported(f"unbound name {name}")

    def get(self, cell, path):
        v = cell.v
        for st in path:
            v = _child(v, st)
        if v is MOVED:
            raise ModelError(f"use of moved place {cell.name}")
        return v

    def deref(self, c, path, b):
        """Follow a reference or shared handle stored at (c, path)."""
        v = self.get(c, path)
        while True:
            if isinstance(v, Ref):
                c, path, b = v.cell, list(v.path), "ref"
            elif isinstance(v, Handle):
                c, path, b = v.box, [], "shared"
            else:
                return c, path, b
            v = self.get(c, path)

    def deref_refs(self, v):
        """Follow references only: a handle behind a reference stays a handle (it is counted, §6.1)."""
        while isinstance(v, Ref):
            v = self.get(v.cell, v.path)
        return v

    def deref_value(self, v):
        while True:
            if isinstance(v, Ref):
                v = self.get(v.cell, v.path)
            elif isinstance(v, Handle):
                v = v.box.v
                if v is MOVED:
                    raise ModelError("use of a released shared value")
            else:
                return v

    PLACES = ("var", "field", "tidx", "index")
    _insert_at = None

    def resolve(self, e):
        """(cell, path, borrowed) for a place expression; a non-place base becomes a temporary."""
        k = e[0]
        if k == "var":
            if e[1] in ("None",) and not any(e[1] in sc.names for sc in self.frame.scopes):
                return None
            if e[1] == "self" or not self.is_fn_name(e[1]):
                return self.lookup(e[1]), [], None
        if k in ("field", "tidx", "index"):
            c, p, b = self.resolve_or_temp(e[1])
            c, p, b = self.deref(c, p, b)
            if k == "field":
                st = ("f", e[2])
            elif k == "tidx":
                st = ("i", e[2])
            else:
                base = self.get(c, p)
                iv = self.read(e[2])
                if isinstance(iv, Prim) and iv.kind == "int":
                    if not isinstance(base, VecV):
                        if isinstance(base, MapV):
                            idx = self.map_find(base, iv)
                            if idx is None:
                                if e is self._insert_at:
                                    return ("mapins", base, iv)
                                raise KPanic("missing map key")
                            return c, p + [("i", idx)], "index"
                        raise Unsupported("index of a non-Vec")
                    if not 0 <= iv.v < len(base.elems):
                        raise KPanic("index out of bounds")
                    return c, p + [("i", iv.v)], "index"
                if isinstance(base, MapV):
                    idx = self.map_find(base, iv)
                    if idx is None:
                        if e is self._insert_at:
                            return ("mapins", base, iv)
                        raise KPanic("missing map key")
                    return c, p + [("i", idx)], "index"
                raise Unsupported("range or non-integer index")
            return c, p + [st], b
        return None

    def is_fn_name(self, name):
        return False

    def resolve_or_temp(self, e):
        pl = self.resolve(e)
        if pl is not None:
            return pl
        c = Cell(self.eval(e), "<temp>", temp=True)
        self.temps[-1].append(c)
        return c, [], None

    def use(self, c, p, b):
        """Use the place as a value (§2.2): copy, count or move."""
        v = self.get(c, p)
        if isinstance(v, Ref):
            return v
        if isinstance(v, Handle):
            v.box.count += 1
            return Handle(v.box)
        if self.is_copy(v):
            return self.copy_value(v)
        if COUNT_HANDLE_AGGREGATES and self.handle_like(v):
            return self.clone_value(v)  # pending decision: Option/tuple of handles counts like a handle
        if b == "index":
            raise ModelError("move out of an index projection (C3)")
        if b:
            raise ModelError(f"move out of a {b} place (C3)")
        if _has_moved(v):
            raise ModelError("use of a partially moved value (C4)")
        # moving a part out of a value whose type has a Drop body (§3.6)
        anc = c.v
        for st in p:
            if self.has_drop(anc):
                raise ModelError("move out of a field of a type with a Drop body (C3)")
            anc = _child(anc, st)
        self.set_raw(c, p, MOVED)
        loops = self.frame.loops
        if 0 <= c.loop_depth < len(loops):
            for d in range(c.loop_depth, len(loops)):
                loops[d].add(c)
        return v

    def back_edge(self):
        """§3.3: a place declared outside the loop and moved in it must be initialized again at every back edge."""
        moved = self.frame.loops[-1]
        for c in moved:
            if _has_moved(c.v):
                raise ModelError(f"`{c.name}` is moved inside a loop and not initialized again before the next iteration (E0500)")
        moved.clear()

    def set_raw(self, c, p, nv):
        if not p:
            c.v = nv
            return
        v = c.v
        for st in p[:-1]:
            v = _child(v, st)
        if v is MOVED:
            raise ModelError("assignment into a moved value")
        _set_child(v, p[-1], nv)

    def raw(self, c, p):
        v = c.v
        for st in p:
            if v is MOVED:
                raise ModelError("assignment into a moved value")
            v = _child(v, st)
        return v

    def value(self, e):
        """Evaluate e for ownership: a place is used (§2.2); anything else is a fresh value."""
        if e[0] in self.PLACES:
            pl = self.resolve(e)
            if pl is not None:
                c, p, b = pl
                if e[0] == "var" and c.refbind:
                    return c.v if not isinstance(c.v, Ref) else c.v
                return self.use(c, p, b)
        return self.eval(e)

    def borrow(self, e, mut=False):
        if e[0] in self.PLACES:
            pl = self.resolve(e)
            if pl is not None:
                c, p, b = pl
                v = self.get(c, p)
                if isinstance(v, Ref):
                    return v
                if _has_moved(v):
                    raise ModelError("borrow of a partially moved value (C4)")
                return Ref(c, p, mut)
        c = Cell(self.eval(e), "<temp>", temp=True)
        self.temps[-1].append(c)
        return Ref(c, [], mut)

    def read(self, e):
        """The value of e for reading only; an rvalue becomes a temporary of the statement."""
        if e[0] in self.PLACES:
            pl = self.resolve(e)
            if pl is not None:
                return self.deref_value(self.get(pl[0], pl[1]))
        v = self.eval(e)
        if not isinstance(v, Prim):
            self.temps[-1].append(Cell(v, "<temp>", temp=True))
        return self.deref_value(v)

    # ── statements ──
    def in_stmt(self, fn, keep_result=False):
        self.temps.append([])
        try:
            r = fn()
        except (Ret, Brk, Cont):
            self.drop_temps(self.temps.pop())
            raise
        except KPanic:
            raise
        ts = self.temps.pop()
        if keep_result:
            self.drop_temps(ts)
            return r
        if r is not None and not isinstance(r, Prim):
            ts.append(Cell(r, "<result>", temp=True))
        self.drop_temps(ts)
        return None

    def exec_stmt(self, s):
        self.steps += 1
        if self.steps > 2_000_000:
            raise Unsupported("step limit")
        k = s[0]
        sc = self.frame.scopes[-1]
        if k == "let":
            _, pat, ty, init = s
            if init is None:
                if pat[0] != "pbind":
                    raise Unsupported("let without initializer")
                sc.bind(pat[1], Cell(MOVED))
                return

            def do_let():
                if pat[0] == "pbind" and not pat[2]:
                    v = self.value(init)
                    if isinstance(v, Ref) and ty is not None and ty[0] not in ("ref", "mutref"):
                        # an owned annotation on a reference: Copy is read, anything else is C3
                        r = self.read_through_ref(self.deref_refs(v))
                        if r is None:
                            raise ModelError("move out of a ref place (C3): owned `let` annotation on a reference")
                        v = r
                    self.check_no_temp_origin(v)
                    c = Cell(v)
                    c.refbind = isinstance(v, Ref)
                    c.ty = ty
                    self.mark_array(v, ty)
                    sc.bind(pat[1], c)
                    return None
                c, p, b = self.resolve_or_temp(init)
                if not self.matches(pat, c, p):
                    raise KPanic("refutable let pattern did not match")
                self.bind(pat, c, p, b, sc, in_let=True)
                return None

            self.in_stmt(do_let)
            return
        if k == "letelse":
            _, pat, ty, init, els = s
            res = {}

            def do_letelse():
                c, p, b = self.resolve_or_temp(init)
                if self.matches(pat, c, p):
                    self.bind(pat, c, p, b, sc, in_let=True)
                    res["ok"] = True
                return None

            self.in_stmt(do_letelse)
            if not res:
                self.eval_block(els)
                raise ModelError("let-else block fell through")
            return
        if k == "defer":
            sc.entries.append(("defer", s[1]))
            return
        if k == "errdefer":
            sc.entries.append(("errdefer", s[1]))
            return
        if k == "expr":
            self.in_stmt(lambda: self.eval(s[1]))
            return
        raise Unsupported(f"statement {k}")

    def eval_block(self, blk: Block, fn_body=False, pre_scope: Optional[Scope] = None):
        sc = pre_scope or Scope()
        self.frame.scopes.append(sc)
        try:
            for s in blk.stmts:
                self.exec_stmt(s)
            if blk.tail is not None:
                v = self.in_stmt(lambda: self.ret_value(blk.tail), keep_result=True)
                if fn_body:
                    v = self.own_returned(v)  # before the body's locals drop: the referent is still live
            else:
                v = UNIT
        except Ret as r:
            self.exit_scope(sc, r.is_err)
            self.frame.scopes.pop()
            raise
        except (Brk, Cont):
            self.exit_scope(sc, False)
            self.frame.scopes.pop()
            raise
        self.exit_scope(sc, fn_body and is_err_result(v))
        self.frame.scopes.pop()
        return v

    # ── calls ──
    def call_user(self, fdef: FnDef, recv, args, self_type):
        if fdef.body is None:
            raise Unsupported(f"call of bodiless fn {fdef.name}")
        frame = Frame([Scope()], self_type, ret_ref=bool(fdef.ret) and fdef.ret[0] in ("ref", "mutref"), ret=fdef.ret)
        frame.own_ret = not frame.ret_ref and fdef.ret is not None and not self.generic_ret(fdef)
        self.frames.append(frame)
        ps = frame.scopes[0]
        if recv is not None:
            c = Cell(recv)
            c.refbind = isinstance(recv, Ref)
            ps.bind("self", c)
        for prm, a in zip(fdef.params, args):
            c = Cell(a)
            c.refbind = isinstance(a, Ref)
            c.ty = prm.ty
            if prm.mode == "own":
                self.mark_array(a, prm.ty)
            ps.bind(prm.name, c)
        try:
            v = self.eval_block(fdef.body, fn_body=True)
            is_err = is_err_result(v)
        except Ret as r:
            v, is_err = r.v, r.is_err
        if isinstance(v, Ref) and not frame.ret_ref and fdef.ret is not None and not self.generic_ret(fdef):
            # an owned return type: a reference flowing out is read (Copy), counted (handle) or a C3 move
            r = self.read_through_ref(self.deref_refs(v))
            if r is not None:
                v = r
            else:
                raise ModelError("move out of a ref place (C3): a reference returned where an owned value is expected")
        self.exit_scope(ps, is_err)
        self.frames.pop()
        refs = [x for x in [recv] + list(args) if isinstance(x, Ref)]
        if isinstance(recv, Ref):
            refs = [recv]
        self.tag_origins(v, [r.cell for r in refs] + [o for r in refs for o in r.origins])
        return v

    def tag_origins(self, v, cells):
        if isinstance(v, Ref):
            v.origins = v.origins + cells
        elif isinstance(v, Enum) and isinstance(v.payload, list):
            for x in v.payload:
                self.tag_origins(x, cells)
        elif isinstance(v, Tup):
            for x in v.elems:
                self.tag_origins(x, cells)

    def check_no_temp_origin(self, v):
        if isinstance(v, Ref):
            if v.cell.temp or any(o.temp for o in v.origins):
                raise ModelError("a reference to a temporary outlives its statement (§5.5)")
        elif isinstance(v, Enum) and isinstance(v.payload, list):
            for x in v.payload:
                self.check_no_temp_origin(x)
        elif isinstance(v, Tup):
            for x in v.elems:
                self.check_no_temp_origin(x)

    def args_for(self, fdef: FnDef, args):
        if len(args) != len(fdef.params):
            raise Unsupported(f"arity mismatch calling {fdef.name}")
        out = []
        for prm, a in zip(fdef.params, args):
            if prm.mode == "own":
                v = self.value(a)
                if isinstance(v, Ref) and not self.generic_param(fdef, prm):
                    r = self.read_through_ref(self.deref_refs(v))
                    if r is not None:
                        v = r
                    else:
                        raise ModelError("move out of a ref place (C3): a reference passed where an owned value is expected")
                out.append(v)
            else:
                out.append(self.borrow(a, prm.mode == "mutref"))
        return out

    def store_owned(self, v, ty, what):
        """A value stored into an owned field or payload: a reference is read (Copy), counted (handle) or C3."""
        if not isinstance(v, Ref):
            return v
        if ty is not None and (ty[0] in ("ref", "mutref") or ty[0] != "app"
                               or (len(ty[1]) == 1 and ty[1] not in self.p.structs and ty[1] not in self.p.enums)):
            return v  # a declared reference, a view, or a generic parameter
        r = self.read_through_ref(self.deref_refs(v))
        if r is not None:
            return r
        raise ModelError(f"move out of a ref place (C3): a reference stored into an owned {what}")

    def own_nested(self, v, ty, what):
        """store_owned applied through an aggregate, following its declared type: a reference inside an
        owned tuple, array, Vec or Option/Result whose declared component is owned is read, counted or C3."""
        if ty is None or ty[0] in ("ref", "mutref"):
            return v
        if isinstance(v, Ref):
            return self.store_owned(v, ty, what)
        if ty[0] == "tuple" and isinstance(v, Tup) and len(ty[1]) == len(v.elems):
            v.elems = [self.own_nested(x, t, what) for x, t in zip(v.elems, ty[1])]
        elif ty[0] == "app" and ty[2]:
            if isinstance(v, VecV) and ty[1] in ("Vec", "Array") and v.kind in ("Vec", "Array"):
                v.elems = [self.own_nested(x, ty[2][0], what) for x in v.elems]
            elif isinstance(v, Enum) and v.ty == ty[1] and isinstance(v.payload, list) and len(v.payload) == 1:
                k = 1 if (ty[1] == "Result" and v.var == "Err" and len(ty[2]) > 1) else 0
                if ty[1] in ("Option", "Result"):
                    v.payload = [self.own_nested(v.payload[0], ty[2][k], what)]
        return v

    def generic_ret(self, fdef):
        t = fdef.ret
        if t is None or t[0] != "app":
            return t is not None and t[0] not in ("tuple",)
        if t[1] in fdef.generics:
            return True
        return len(t[1]) == 1 and t[1] not in self.p.structs and t[1] not in self.p.enums

    def generic_param(self, fdef, prm):
        t = prm.ty
        if t is None or t[0] != "app":
            return False
        if t[1] in fdef.generics:
            return True
        # an impl-level parameter is not in fdef.generics; a one-letter name that is not a declared type is one
        return len(t[1]) == 1 and t[1] not in self.p.structs and t[1] not in self.p.enums

    def type_name(self, v):
        v = v
        if isinstance(v, Handle):
            return v.box.v.ty
        if isinstance(v, (Rec, Enum)):
            return v.ty
        if isinstance(v, Prim):
            return {"int": "i64", "float": "f64", "bool": "bool", "char": "char", "unit": "unit"}[v.kind]
        if isinstance(v, Str):
            return "String"
        if isinstance(v, VecV):
            return v.kind
        if isinstance(v, MapV):
            return v.kind
        if isinstance(v, Tup):
            return "tuple"
        return type(v).__name__

    def find_method(self, tname, name):
        ms = self.p.methods.get(tname)
        if ms and name in ms:
            return ms[name]
        if tname == "i64":
            for t in INT_TYPES:
                ms = self.p.methods.get(t)
                if ms and name in ms:
                    return ms[name]
        return None

    # ── expressions ──
    def eval(self, e):
        h = getattr(self, "e_" + e[0], None)
        if h is None:
            raise Unsupported(f"expression {e[0]}")
        return h(e)

    def e_int(self, e):
        return Prim(e[1], "int")

    def e_float(self, e):
        return Prim(e[1], "float")

    def e_bool(self, e):
        return Prim(e[1], "bool")

    def e_char(self, e):
        return Prim(e[1], "char")

    def e_unit(self, e):
        return UNIT

    def e_str(self, e):
        return Str(e[1])

    def e_fstr(self, e):
        parts = []
        for part in e[1]:
            if isinstance(part, str):
                parts.append(part)
            else:
                ex, spec = part
                parts.append(self.fmt(self.read(ex), spec))
        return Str("".join(parts))

    def e_var(self, e):
        name = e[1]
        if name == "None":
            return none()
        if name in self.p.fns and not any(name in sc.names for sc in self.frame.scopes):
            raise Unsupported("function values")
        pl = self.resolve(e)
        return self.use(*pl)

    e_field = e_var

    def e_field(self, e):
        return self.use(*self.resolve(e))

    def e_tidx(self, e):
        return self.use(*self.resolve(e))

    def e_index(self, e):
        return self.use(*self.resolve(e))

    def e_path(self, e):
        path = e[1]
        if len(path) == 2:
            t, v = path
            if t == "Self":
                t = self.frame.self_type
            ed = self.p.enums.get(t)
            if ed is not None and v in ed.variants:
                if ed.variants[v] not in (None, []):
                    raise Unsupported("enum constructor as a value")
                return self.wrap_shared(Enum(t, v, None))
            if t == "Option" and v == "None":
                return none()
            if t in ("i64", "i32", "u64", "u32", "usize", "isize", "u8") and v in ("MAX", "MIN"):
                bits = int(t[1:]) if t[1:].isdigit() else 64
                if t.startswith("i"):
                    return Prim(2 ** (bits - 1) - 1 if v == "MAX" else -(2 ** (bits - 1)), "int")
                return Prim(2 ** bits - 1 if v == "MAX" else 0, "int")
        raise Unsupported(f"path {'.'.join(path)}")

    def wrap_shared(self, v):
        if isinstance(v, (Rec, Enum)) and self.is_shared_type(v.ty):
            box = Cell(v, "<shared>")
            box.count = 1
            return Handle(box)
        return v

    def e_struct(self, e):
        _, name, fs, base = e
        if base is not None:
            raise Unsupported("struct update syntax")
        if name == "Self":
            name = self.frame.self_type
        if "." in name:
            t, var = name.split(".", 1)
            if t == "Self":
                t = self.frame.self_type
            ed = self.p.enums.get(t)
            if ed is None:
                raise Unsupported(f"struct literal {name}")
            ftys = dict(ed.variants[var])
            vals = {f: self.store_owned(self.value(x), ftys.get(f), "enum payload") for f, x in fs}
            order = [f for f, _ in ed.variants[var]]
            return self.wrap_shared(Enum(t, var, {f: vals[f] for f in order}))
        sd = self.p.structs.get(name)
        if sd is None:
            raise Unsupported(f"struct literal of unknown type {name}")
        vals = {}
        ftys = dict(sd.fields)
        for f, x in fs:  # written order (§8.1)
            vals[f] = self.store_owned(self.value(x), ftys.get(f), "struct field")
        missing = [f for f, _ in sd.fields if f not in vals]
        if missing:
            raise Unsupported(f"missing fields {missing}")
        return self.wrap_shared(Rec(name, {f: vals[f] for f, _ in sd.fields}))

    def e_tuple(self, e):
        return Tup([self.value(x) for x in e[1]])

    def e_array(self, e):
        return VecV([self.value(x) for x in e[1]])

    def e_arrayrep(self, e):
        v = self.value(e[1])
        n = self.read(e[2]).v
        if not self.is_copy(v) and n != 1:
            if isinstance(v, (Str, VecV)) and not self.observable(v):
                return VecV([self.clone_value(v) for _ in range(n)])
            raise Unsupported("array repeat of a move-only value")
        return VecV([self.copy_value(v) if self.is_copy(v) else v for _ in range(n)])

    def observable(self, v) -> bool:
        if isinstance(v, (Handle,)):
            return True
        if isinstance(v, (Rec, Enum)) and v.ty in self.p.drops:
            return True
        if isinstance(v, Rec):
            return any(self.observable(x) for x in v.fields.values())
        if isinstance(v, (Tup, VecV)):
            return any(self.observable(x) for x in v.elems)
        return False

    def e_cast(self, e):
        v = self.read(e[1])
        t = e[2][1] if e[2][0] == "app" else None
        if t in ("f64", "f32"):
            x = float(ord(v.v) if v.kind == "char" else v.v)
            if t == "f32":
                import struct
                try:
                    x = struct.unpack("f", struct.pack("f", x))[0]
                except OverflowError:
                    x = float("inf") if x > 0 else float("-inf")
            return Prim(x, "float")
        if t in INT_TYPES:
            if v.kind == "char":
                x = ord(v.v)
            elif v.kind == "bool":
                x = int(v.v)
            elif v.kind == "float":
                # Rust `as`: truncate toward zero, saturate at the target's bounds, NaN is 0
                import math
                f = v.v
                lo, hi = {"i8": (-2**7, 2**7 - 1), "u8": (0, 2**8 - 1), "i16": (-2**15, 2**15 - 1), "u16": (0, 2**16 - 1),
                          "i32": (-2**31, 2**31 - 1), "u32": (0, 2**32 - 1)}.get(t, (-2**63, 2**63 - 1))
                if t in ("u64", "usize", "u128", "i128"):
                    raise Unsupported("integer width (the model's integers are i64)")
                return Prim(0 if math.isnan(f) else max(lo, min(hi, int(f) if math.isfinite(f) else (hi if f > 0 else lo))), "int")
            else:
                x = int(v.v)
            bits = {"i8": 8, "u8": 8, "i16": 16, "u16": 16, "i32": 32, "u32": 32}.get(t)
            if bits is not None:  # `as` truncates to the target width
                x &= (1 << bits) - 1
                if t[0] == "i" and x >= 1 << (bits - 1):
                    x -= 1 << bits
            elif t in ("u64", "usize") and x < 0:
                raise Unsupported("integer width (the model's integers are i64)")
            return Prim(x, "int")
        if t == "char":
            return Prim(chr(v.v), "char")
        raise Unsupported(f"cast to {t}")

    def e_range(self, e):
        lo = self.read(e[1]).v
        hi = self.read(e[2]).v if e[2] is not None else None
        if hi is None:
            raise Unsupported("open range")
        if e[3]:
            hi += 1
        return ("range", lo, hi)

    def e_un(self, e):
        v = self.read(e[2])
        if not isinstance(v, Prim):
            raise Unsupported(f"unary {e[1]} on {type(v).__name__} (ill-typed)")
        if e[1] == "-":
            if v.kind == "int" and -v.v >= 2 ** 63:
                raise KPanic("integer overflow")
            return Prim(-v.v, v.kind)
        return Prim(not v.v, "bool")

    def e_bin(self, e):
        op, a, b = e[1], e[2], e[3]
        if op == "&&":
            return Prim(bool(self.read(a).v) and bool(self.read(b).v), "bool")
        if op == "||":
            return Prim(bool(self.read(a).v) or bool(self.read(b).v), "bool")
        x = self.read(a)
        y = self.read(b)
        u = self.user_compare(op, x, y)
        if u is not None:
            return Prim(u, "bool")
        if op in ("==", "!="):
            r = self.equal(x, y)
            return Prim(r if op == "==" else not r, "bool")
        if op in ("<", ">", "<=", ">="):
            px, py = self.deref_value(x), self.deref_value(y)
            if isinstance(px, Prim) and isinstance(py, Prim):
                kx, ky = px.v, py.v  # IEEE: every ordering against NaN is false
            else:
                kx, ky = self.sort_key(x), self.sort_key(y)
            r = {"<": kx < ky, ">": kx > ky, "<=": kx <= ky, ">=": kx >= ky}[op]
            return Prim(r, "bool")
        if isinstance(x, Str) or isinstance(y, Str):
            if op != "+":
                raise Unsupported(f"string {op}")
            return Str(self.fmt(x, "") + self.fmt(y, ""))
        if not (isinstance(x, Prim) and isinstance(y, Prim)):
            raise Unsupported(f"operator {op} on {type(x).__name__}")
        p, q = x.v, y.v
        kind = "float" if "float" in (x.kind, y.kind) else x.kind
        if op == "+":
            r = p + q
        elif op == "-":
            r = p - q
        elif op == "*":
            r = p * q
        elif op == "/":
            if kind == "float":
                r = p / q if q != 0 else (float("inf") if p > 0 else float("-inf") if p < 0 else float("nan"))
            else:
                if q == 0:
                    raise KPanic("division by zero")
                r = abs(p) // abs(q) * (1 if (p >= 0) == (q >= 0) else -1)
        elif op == "%":
            if kind == "float":
                import math
                r = math.fmod(p, q)
            else:
                if q == 0:
                    raise KPanic("remainder by zero")
                if p == -(2 ** 63) and q == -1:
                    raise KPanic("integer overflow")
                r = abs(p) % abs(q) * (1 if p >= 0 else -1)
        elif op == "&":
            r = p & q
        elif op == "|":
            r = p | q
        elif op == "^":
            r = p ^ q
        elif op == "<<":
            r = p << q
        elif op == ">>":
            r = p >> q
        else:
            raise Unsupported(op)
        if kind == "int" and not (-(2 ** 63) <= r < 2 ** 63):
            raise KPanic("integer overflow")
        if kind == "bool":
            return Prim(bool(r), "bool")
        return Prim(r, kind)

    def equal(self, x, y) -> bool:
        if self.user_type(x):
            raise Unsupported("equality or hashing through a hand-written impl outside an operator")
        x, y = self.deref_value(x), self.deref_value(y)
        if isinstance(x, Prim) and isinstance(y, Prim):
            return x.v == y.v
        if isinstance(x, Str) and isinstance(y, Str):
            return x.s == y.s
        if isinstance(x, Enum) and isinstance(y, Enum):
            if (x.ty, x.var) != (y.ty, y.var):
                return False
            if isinstance(x.payload, dict):
                return all(self.equal(x.payload[k], y.payload[k]) for k in x.payload)
            return all(self.equal(p, q) for p, q in zip(x.payload or [], y.payload or []))
        if isinstance(x, Rec) and isinstance(y, Rec):
            return x.ty == y.ty and all(self.equal(x.fields[k], y.fields[k]) for k in x.fields)
        if isinstance(x, (Tup, VecV)) and isinstance(y, (Tup, VecV)):
            return len(x.elems) == len(y.elems) and all(self.equal(p, q) for p, q in zip(x.elems, y.elems))
        if isinstance(x, Str) or isinstance(y, Str):
            return False
        raise Unsupported(f"equality on {type(x).__name__}")

    USER_CMP = ("eq", "ne", "partial_cmp", "cmp", "lt", "le", "gt", "ge", "hash")

    def user_type(self, x):
        """The name of a user type with a hand-written comparison or hash impl, else None."""
        x = self.deref_value(x)
        if isinstance(x, Handle):
            x = x.box.v
        if isinstance(x, (Rec, Enum)) and x.ty in self.p.methods:
            if any(m in self.p.methods[x.ty] for m in self.USER_CMP):
                return x.ty
        return None

    def user_compare(self, op, x, y):
        """A comparison operator on a type with its own impl calls the impl (§4.7); None if structural."""
        if op not in ("==", "!=", "<", ">", "<=", ">="):
            return None
        t = self.user_type(x)
        if t is None:
            return None
        rx = Ref(Cell(self.deref_value(x), "<cmp>", temp=True), [])
        ry = Ref(Cell(self.deref_value(y), "<cmp>", temp=True), [])
        if op in ("==", "!="):
            f = self.find_method(t, "eq")
            if f is None:
                raise Unsupported(f"derived equality on {t} beside a hand-written ordering")
            r = bool(self.call_user(f, rx, [ry], t).v)
            return r if op == "==" else not r
        f = self.find_method(t, "partial_cmp") or self.find_method(t, "cmp")
        if f is None:
            raise Unsupported(f"ordering on {t} without partial_cmp/cmp")
        o = self.deref_value(self.call_user(f, rx, [ry], t))
        if isinstance(o, Enum) and o.ty == "Option":
            if o.var == "None":
                return False
            o = self.deref_value(o.payload[0])
        if not (isinstance(o, Enum) and o.var in ("Less", "Equal", "Greater")):
            raise Unsupported("comparison impl returned a non-Ordering")
        return {"<": o.var == "Less", ">": o.var == "Greater",
                "<=": o.var != "Greater", ">=": o.var != "Less"}[op]

    def map_add(self, m, k, v):
        """A new entry: Sorted* keep key order, Map/Set insertion order (their walk is unordered anyway)."""
        i = len(m.keys)
        if m.kind in ("SortedMap", "SortedSet"):
            kk = self.sort_key(k)
            i = next((j for j, x in enumerate(m.keys) if self.sort_key(x) > kk), len(m.keys))
        m.keys.insert(i, k)
        m.vals.insert(i, v)

    def sort_key(self, x):
        if self.user_type(x):
            raise Unsupported("sorting or keying by a hand-written comparison impl")
        x = self.deref_value(x)
        if isinstance(x, Prim):
            return (0, x.v)
        if isinstance(x, Str):
            return (1, x.s)
        if isinstance(x, (Tup, VecV)):
            return (2, tuple(self.sort_key(y) for y in x.elems))
        if isinstance(x, Rec):
            return (3, tuple(self.sort_key(y) for y in x.fields.values()))
        if isinstance(x, Enum):
            ed = self.p.enums.get(x.ty)
            order = ed.order if ed else ["None", "Some", "Ok", "Err"]
            pl = x.payload.values() if isinstance(x.payload, dict) else (x.payload or [])
            return (4, order.index(x.var), tuple(self.sort_key(y) for y in pl))
        raise Unsupported(f"ordering on {type(x).__name__}")

    # ── formatting ──
    def fmt(self, v, spec):
        v = self.deref_value(v)
        debug = spec.endswith("?")
        if debug:
            spec = spec[:-1]
        s = self.debug(v) if debug else self.display(v, spec)
        if spec:
            import re
            m = re.match(r"^(.?[<>^])?(\+)?(0)?(\d+)?(\.\d+)?$", spec)
            if not m:
                raise Unsupported(f"format spec {spec!r}")
            align, plus, zero, width, prec = m.groups()
            if plus and isinstance(v, Prim) and v.kind in ("int", "float") and v.v >= 0:
                s = "+" + s
            if width:
                w = int(width)
                if align:
                    fill = align[0] if len(align) == 2 else " "
                    a = align[-1]
                    if a == "<":
                        s = s.ljust(w, fill)
                    elif a == ">":
                        s = s.rjust(w, fill)
                    else:
                        s = s.center(w, fill)
                elif zero:
                    neg = s.startswith("-")
                    s = ("-" if neg else "") + s.lstrip("-").rjust(w - neg, "0")
                elif isinstance(v, Prim) and v.kind in ("int", "float"):
                    s = s.rjust(w)
                else:
                    s = s.ljust(w)
        return s

    def display(self, v, spec=""):
        import re
        if isinstance(v, Prim):
            if v.kind == "bool":
                return "true" if v.v else "false"
            if v.kind == "float":
                m = re.search(r"\.(\d+)", spec or "")
                if m:
                    return f"{v.v:.{int(m.group(1))}f}"
                x = v.v
                if x != x:
                    return "NaN"
                if x in (float("inf"), float("-inf")):
                    return "inf" if x > 0 else "-inf"
                if x == int(x) and abs(x) < 1e16:
                    return str(int(x))
                return repr(x)
            if v.kind == "unit":
                return "()"
            return str(v.v)
        if isinstance(v, Str):
            return v.s
        tn = self.type_name(v)
        f = self.find_method(tn, "fmt") or self.find_method(tn, "to_string")
        if f is not None:
            raise Unsupported("user Display impl")
        raise Unsupported(f"display of {tn}")

    def debug(self, v):
        v = self.deref_value(v)
        if isinstance(v, Str):
            return '"' + v.s.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n") + '"'
        if isinstance(v, Prim):
            if v.kind == "char":
                return repr(v.v)
            if v.kind == "float":
                x = v.v
                return f"{x:.1f}" if x == int(x) else repr(x)
            return self.display(v)
        if isinstance(v, VecV):
            return "[" + ", ".join(self.debug(x) for x in v.elems) + "]"
        if isinstance(v, Tup):
            return "(" + ", ".join(self.debug(x) for x in v.elems) + ")"
        if isinstance(v, Enum):
            if v.payload is None:
                return v.var
            if isinstance(v.payload, dict):
                return f"{v.var} {{ " + ", ".join(f"{k}: {self.debug(x)}" for k, x in v.payload.items()) + " }"
            return v.var + "(" + ", ".join(self.debug(x) for x in v.payload) + ")"
        if isinstance(v, Rec):
            return f"{v.ty} {{ " + ", ".join(f"{k}: {self.debug(x)}" for k, x in v.fields.items()) + " }"
        raise Unsupported(f"debug of {type(v).__name__}")

    # ── control flow ──
    def truth(self, e) -> bool:
        res = {}

        def go():
            res["v"] = bool(self.read(e).v)

        self.in_stmt(go, keep_result=True)
        return res["v"]

    def e_if(self, e):
        _, cond, then, els = e
        if cond[0] == "let":
            v = self.if_let(cond[1], cond[2], then, els)
        elif self.truth(cond):
            v = self.eval_block(then)
        elif els is None:
            return UNIT
        else:
            v = self.eval_block(els)
        if isinstance(v, Ref) and els is not None and (self.owned_expr(("block", then)) or self.owned_expr(("block", els))):
            # the branches unify to the value type (one is plainly owned): read, count or C3, as for `match`
            r = self.read_through_ref(self.deref_refs(v))
            if r is None:
                raise ModelError("move out of a ref place (C3): an if branch yields a reference where another yields an owned value")
            v = r
        return v

    def if_let(self, pat, scrut, then, els):
        c, p, b = self.resolve_or_temp(scrut)
        if self.matches(pat, c, p):
            sc = Scope()
            self.bind(pat, c, p, b, sc)
            return self.eval_block(then, pre_scope=sc)
        if els is None:
            return UNIT
        return self.eval_block(els)

    def e_block(self, e):
        return self.eval_block(e[1])

    LITERALS = ("int", "float", "bool", "str", "char", "unit")

    def owned_expr(self, e):
        """An arm body that plainly yields an owned value: a literal, an f-string, a struct literal or a clone."""
        if e[0] == "block":
            t = e[1].tail
            return t is not None and self.owned_expr(t)
        if e[0] in self.LITERALS or e[0] in ("fstr", "struct", "array", "tuple"):
            return True
        return e[0] == "mcall" and e[2] in ("to_string", "clone", "to_owned") and not e[3]

    def e_match(self, e):
        _, scrut, arms = e
        c, p, b = self.resolve_or_temp(scrut)
        for pat, guard, body in arms:
            if not self.matches(pat, c, p):
                continue
            if guard is not None:
                gs = Scope()
                self.bind(pat, c, p, "guard", gs)
                self.frame.scopes.append(gs)
                try:
                    ok_ = self.truth(guard)
                finally:
                    self.frame.scopes.pop()
                if not ok_:
                    continue
            sc = Scope()
            self.bind(pat, c, p, b, sc)
            self.frame.scopes.append(sc)
            try:
                v = self.value(body) if body[0] != "block" else self.eval_block(body[1])
            except Ret as r:
                self.exit_scope(sc, r.is_err)
                self.frame.scopes.pop()
                raise
            except (Brk, Cont):
                self.exit_scope(sc, False)
                self.frame.scopes.pop()
                raise
            if isinstance(v, Ref) and any(self.owned_expr(b) for _, _, b in arms):
                # arms unify to the value type (another arm is plainly owned): a Copy referent is read,
                # anything else would move out of the reference (C3)
                r = self.read_through_ref(self.deref_refs(v))
                if r is not None:
                    v = r
                else:
                    self.exit_scope(sc, False)
                    self.frame.scopes.pop()
                    raise ModelError("move out of a ref place (C3): a match arm yields a reference where another yields an owned value")
            self.exit_scope(sc, False)
            self.frame.scopes.pop()
            return v
        raise KPanic("non-exhaustive match")

    def e_loop(self, e):
        fr = self.frame
        fr.loops.append(set())
        try:
            while True:
                try:
                    self.eval_block(e[1])
                except Brk as b:
                    return b.v if b.v is not None else UNIT
                except Cont:
                    pass
                self.back_edge()
        finally:
            fr.loops.pop()

    def e_while(self, e):
        fr = self.frame
        fr.loops.append(set())
        try:
            return self.while_(e)
        finally:
            fr.loops.pop()

    def while_(self, e):
        _, cond, body = e
        first = True
        while True:
            if not first:
                self.back_edge()
            first = False
            if cond[0] == "let":
                pat, scrut = cond[1], cond[2]
                self.temps.append([])
                try:
                    c, p, b = self.resolve_or_temp(scrut)
                    if not self.matches(pat, c, p):
                        self.drop_temps(self.temps.pop())
                        return UNIT
                    sc = Scope()
                    self.bind(pat, c, p, b, sc)
                    try:
                        self.eval_block(body, pre_scope=sc)
                    except Cont:
                        pass
                except Brk:
                    self.drop_temps(self.temps.pop())
                    return UNIT
                except (Ret,):
                    self.drop_temps(self.temps.pop())
                    raise
                self.drop_temps(self.temps.pop())
                continue
            if not self.truth(cond):
                return UNIT
            try:
                self.eval_block(body)
            except Brk:
                return UNIT
            except Cont:
                continue

    def e_for(self, e):
        _, pat, it, body = e
        items = self.iterate(it)
        fr = self.frame
        fr.loops.append(set())
        first = True
        try:
            for item in items:
                if not first:
                    self.back_edge()
                first = False
                sc = Scope()
                holder = Cell(item, "<item>", temp=True)
                b = "ref" if isinstance(item, Ref) else None
                if not self.matches(pat, holder, []):
                    raise KPanic("for pattern did not match")
                self.bind(pat, holder, [], None, sc)
                self.drop_value(holder.v) if holder.v is not MOVED and not isinstance(holder.v, Ref) else None
                try:
                    self.eval_block(body, pre_scope=sc)
                except Cont:
                    continue
        except Brk:
            pass
        finally:
            fr.loops.pop()
            if hasattr(items, "close"):
                items.close()
        return UNIT

    def iterate(self, it):
        """A generator of items; owned items left over drop when it closes (§7.4: `for` iterator)."""
        if it[0] == "range":
            lo = self.read(it[1]).v
            hi = self.read(it[2]).v + (1 if it[3] else 0)
            return (Prim(i, "int") for i in range(lo, hi))
        if it[0] == "mcall":
            recv, name, args = it[1], it[2], it[3]
            if name == "into_iter" and not args:
                return self.owned_iter(self.value(recv))
            if name in ("iter", "iter_mut") and not args:
                return self.ref_iter(recv)
            if name == "enumerate" and not args:
                inner = self.iterate(recv) if recv[0] in ("mcall",) else self.ref_iter(recv)
                return (Tup([Prim(i, "int"), x]) for i, x in enumerate(inner))
            if name == "rev" and not args:
                return reversed(list(self.iterate(recv)))
            if name == "chars" and not args:
                s = self.read(recv)
                return (Prim(ch, "char") for ch in s.s)
            if name in ("keys", "values") and not args:
                c, p, b = self.resolve_or_temp(recv)
                c, p, b = self.deref(c, p, b)
                m = self.get(c, p)
                n = len(m.keys)
                if name == "keys":
                    return (Ref(c, p + [("k", i)]) for i in range(n))
                return (Ref(c, p + [("i", i)]) for i in range(n))
            raise Unsupported(f"for over .{name}()")
        return self.ref_iter(it)

    def ref_iter(self, e):
        c, p, b = self.resolve_or_temp(e)
        c, p, b = self.deref(c, p, b)
        v = self.get(c, p)
        if isinstance(v, tuple) and v[0] == "range":
            return (Prim(i, "int") for i in range(v[1], v[2]))
        if isinstance(v, VecV):
            def gen():
                i = 0
                while i < len(self.get(c, p).elems):
                    yield Ref(c, p + [("i", i)])
                    i += 1
            return gen()
        if isinstance(v, MapV):
            if v.kind in ("Set", "SortedSet"):
                return (Ref(c, p + [("k", i)]) for i in range(len(v.keys)))
            return (Tup([Ref(c, p + [("k", i)]), Ref(c, p + [("i", i)])]) for i in range(len(v.keys)))
        if isinstance(v, Str):
            raise Unsupported("for over a String")
        raise Unsupported(f"for over {type(v).__name__}")

    def owned_iter(self, v):
        v = self.deref_value(v) if isinstance(v, Ref) else v
        if isinstance(v, tuple) and v[0] == "range":
            return (Prim(i, "int") for i in range(v[1], v[2]))
        if not isinstance(v, VecV):
            raise Unsupported(f"into_iter over {type(v).__name__}")
        model = self

        def gen():
            i = 0
            try:
                while i < len(v.elems):
                    x = v.elems[i]
                    v.elems[i] = MOVED
                    i += 1
                    yield x
            finally:
                # the iterator drops at loop exit: what it still holds drops first to last
                for j in range(i, len(v.elems)):
                    x = v.elems[j]
                    v.elems[j] = MOVED
                    model.drop_value(x)
        return gen()

    def ret_value(self, e):
        if self.frame.ret_ref and e[0] in self.PLACES:
            return self.borrow(e)
        return self.value(e)

    def e_ret(self, e):
        v = UNIT if e[1] is None else self.own_returned(self.ret_value(e[1]))
        raise Ret(v, is_err_result(v))

    def own_returned(self, v):
        """A reference leaving a function whose return type is owned is read (Copy), counted (handle or
        handle aggregate, §6.1) or a C3 move, at the return point, while its referent is still live."""
        if not self.frame.own_ret:
            return v
        if not isinstance(v, Ref):
            return self.own_nested(v, self.frame.ret, "return value")
        r = self.read_through_ref(self.deref_refs(v))
        if r is None:
            raise ModelError("move out of a ref place (C3): a reference returned where an owned value is expected")
        return r

    def e_break(self, e):
        raise Brk(None if e[1] is None else self.value(e[1]))

    def e_continue(self, e):
        raise Cont()

    def e_try(self, e):
        v = self.value(e[1])
        if isinstance(v, Enum) and v.ty == "Result":
            if v.var == "Ok":
                return v.payload[0]
            ev = v.payload[0]
            rt = self.frame.ret
            if rt and rt[0] == "app" and rt[1] == "Result" and len(rt[2]) == 2 and rt[2][1][0] == "app":
                target = rt[2][1][1]
                src_t = self.type_name(ev)
                if target != src_t:
                    f = self.p.from_impls.get((target, src_t))
                    if f is None:
                        raise Unsupported(f"? conversion {src_t} -> {target}")
                    ev = self.call_user(f, None, [ev], target)
            raise Ret(err(ev), True)
        if isinstance(v, Enum) and v.ty == "Option":
            if v.var == "Some":
                return v.payload[0]
            raise Ret(none(), False)
        raise Unsupported("? on a non-Option/Result")

    def e_closure(self, e):
        raise Unsupported("closures")

    def e_assign(self, e):
        _, op, place, rhs = e
        self._insert_at = place if (op == "=" and place[0] == "index") else None
        try:
            pl = self.resolve(place)
        finally:
            self._insert_at = None
        if pl is None:
            raise Unsupported("assignment to a non-place")
        if pl[0] == "mapins":  # `m[k] = v` on a fresh key inserts (legacy behaviour; design.md is silent)
            _, m, k = pl
            v = self.value(rhs)
            self.map_add(m, self.clone_value(k) if isinstance(k, Str) else k, v)
            return UNIT
        c, p, b = pl
        if place[0] == "var" and c.refbind:
            c, p, b = self.deref(c, p, b)
        if p and b == "ref" and False:
            pass
        v = self.value(rhs)
        if op == "=":
            old = self.raw(c, p)
            self.set_raw(c, p, MOVED)
            if old is not MOVED:
                self.drop_value(old)
            self.set_raw(c, p, v)
            return UNIT
        cur = self.deref_value(self.get(c, p))
        bop = op[:-1]
        if isinstance(cur, Str):
            if bop != "+":
                raise Unsupported(f"string {op}")
            self.set_raw(c, p, Str(cur.s + self.fmt(self.deref_value(v), "")))
            return UNIT
        tmp = Model.__new__(Model)
        r = self.e_bin(("bin", bop, ("__v", cur), ("__v", self.deref_value(v))))
        self.set_raw(c, p, r)
        return UNIT

    def e___v(self, e):
        return e[1]

    # ── patterns ──
    def matches(self, pat, c, p) -> bool:
        k = pat[0]
        if k in ("pwild", "pbind", "prest"):
            return True
        c, p, _ = self.deref(c, p, None)
        v = self.get(c, p)
        if k == "plit":
            lit = pat[1]
            if isinstance(v, Str):
                return v.s == lit
            return v.v == lit
        if k == "prange":
            return pat[1] <= v.v <= pat[2]
        if k == "por":
            return any(self.matches(q, c, p) for q in pat[1])
        if k == "pat_at":
            return self.matches(pat[2], c, p)
        if k == "ptuple":
            if not isinstance(v, Tup):
                raise Unsupported("tuple pattern on a non-tuple")
            return all(self.matches(q, c, p + [("i", i)]) for i, q in enumerate(self.expand_rest(pat[1], len(v.elems))) if q is not None)
        if k == "penum":
            var, ty = self.variant_of(pat[1])
            if not isinstance(v, Enum):
                raise Unsupported(f"enum pattern on {type(v).__name__}")
            if v.var != var:
                return False
            subs = pat[2] or []
            n = len(v.payload) if isinstance(v.payload, list) else 0
            return all(self.matches(q, c, p + [("i", i)]) for i, q in enumerate(self.expand_rest(subs, n)) if q is not None)
        if k == "pstruct":
            path = pat[1]
            if len(path) == 2 or path[0] in self.p.enums or (isinstance(v, Enum) and path[0] not in self.p.structs):
                var, ty = self.variant_of(path)
                if not isinstance(v, Enum) or v.var != var:
                    return False
            return all(self.matches(q, c, p + [("f", f)]) for f, q in pat[2])
        raise Unsupported(f"pattern {k}")

    def expand_rest(self, subs, n):
        if any(q[0] == "prest" for q in subs):
            j = next(i for i, q in enumerate(subs) if q[0] == "prest")
            before, after = subs[:j], subs[j + 1:]
            return before + [None] * (n - len(before) - len(after)) + after
        return subs

    def variant_of(self, path):
        if len(path) == 1:
            if path[0] in ("Some", "None"):
                return path[0], "Option"
            if path[0] in ("Ok", "Err"):
                return path[0], "Result"
            for ed in self.p.enums.values():
                if path[0] in ed.variants:
                    return path[0], ed.name
            raise Unsupported(f"unknown pattern name {path[0]}")
        ty = path[-2] if path[-2] != "Self" else self.frame.self_type
        return path[-1], ty

    def bind(self, pat, c, p, b, sc: Scope, in_let=False):
        k = pat[0]
        if k in ("pwild", "plit", "prange", "prest"):
            return
        if k == "pbind":
            _, name, by_ref, mut = pat
            v = self.get(c, p)
            if b == "guard":
                nc = Cell(self.copy_value(v) if self.is_copy(v) else Ref(c, p), refbind=True)
            elif by_ref:
                if in_let and c.temp:
                    raise ModelError("a reference to a temporary outlives its statement (§5.5)")
                nc = Cell(Ref(c, p, mut), refbind=True)
            elif isinstance(v, Ref) or isinstance(v, Handle) or self.is_copy(v):
                nc = Cell(self.use(c, p, b))
                nc.refbind = isinstance(nc.v, Ref)
            elif b:
                # a binding into a borrowed scrutinee is a reference (§4.6)
                nc = Cell(Ref(c, p), refbind=True)
            else:
                nc = Cell(self.use(c, p, b))
            sc.bind(name, nc)
            return
        if k == "por":
            for q in pat[1]:
                if self.matches(q, c, p):
                    self.bind(q, c, p, b, sc, in_let)
                    return
            return
        if k == "pat_at":
            raise Unsupported("@ patterns")
        c, p, b2 = self.deref(c, p, b)
        b = b2 if b2 else b
        v = self.get(c, p)
        if k == "ptuple":
            for i, q in enumerate(self.expand_rest(pat[1], len(v.elems))):
                if q is not None:
                    self.bind(q, c, p + [("i", i)], b, sc, in_let)
            return
        if k == "penum":
            subs = pat[2] or []
            if subs and self.has_drop(v) and self.binds_a_move(subs, v, b):
                raise ModelError("move out of the payload of a type with a Drop body (C3)")
            n = len(v.payload) if isinstance(v.payload, list) else 0
            for i, q in enumerate(self.expand_rest(subs, n)):
                if q is not None:
                    self.bind(q, c, p + [("i", i)], b, sc, in_let)
            return
        if k == "pstruct":
            if self.has_drop(v) and not b and any(
                    q[0] == "pbind" and not q[2] and not self.is_copy(self.get(c, p + [("f", f)]))
                    or q[0] not in ("pwild", "prest", "pbind", "plit", "prange")
                    for f, q in pat[2]):
                raise ModelError("move out of a field of a type with a Drop body (C3)")
            for f, q in pat[2]:
                self.bind(q, c, p + [("f", f)], b, sc, in_let)
            return
        raise Unsupported(f"bind {k}")

    def binds_a_move(self, subs, v, b):
        if b:
            return False
        for i, q in enumerate(subs):
            if q[0] in ("pwild", "prest", "plit", "prange") or (q[0] == "pbind" and q[2]):
                continue
            if q[0] == "pbind" and isinstance(v.payload, list) and i < len(v.payload) and self.is_copy(v.payload[i]):
                continue
            return True
        return False

    # ── calls and methods ──
    def e_call(self, e):
        _, callee, args = e
        if callee[0] == "var":
            name = callee[1]
            for sc in reversed(self.frame.scopes):
                if name in sc.names:
                    raise Unsupported("call of a local (closure)")
            f = self.p.fns.get(name)
            if f is not None:
                return self.call_user(f, None, self.args_for(f, args), None)
            return self.builtin_call(name, args)
        if callee[0] == "path":
            path = callee[1]
            if len(path) == 2:
                t, m = path
                if t == "Self":
                    t = self.frame.self_type
                ed = self.p.enums.get(t)
                if ed is not None and m in ed.variants:
                    tys = ed.variants[m] or []
                    vals = [self.value(a) for a in args]
                    vals = [self.store_owned(x, tys[i] if i < len(tys) else None, "enum payload") for i, x in enumerate(vals)]
                    return self.wrap_shared(Enum(t, m, vals))
                if t == "Option" and m == "Some":
                    return some(self.value(args[0]))
                if t == "Result" and m in ("Ok", "Err"):
                    return self.builtin_call(m, args)
                f = self.find_method(t, m)
                if f is not None:
                    if f.recv is not None:
                        # Type.method(recv, args): UFCS
                        rmode = f.recv
                        r = self.value(args[0]) if rmode == "own" else self.borrow(args[0], rmode == "mutref")
                        return self.call_user(f, r, self.args_for(f, args[1:]), t)
                    return self.call_user(f, None, self.args_for(f, args), t)
                return self.builtin_static(t, m, args)
            raise Unsupported(f"call of path {'.'.join(path)}")
        raise Unsupported(f"call of {callee[0]}")

    def builtin_call(self, name, args):
        if name in ("println", "print", "eprintln", "eprint"):
            if len(args) == 0:
                s = ""
            elif len(args) == 1:
                s = self.fmt(self.read(args[0]), "")
            else:
                raise Unsupported("println with several arguments")
            if name.startswith("e"):
                return UNIT
            self.out.append(s + ("\n" if name.endswith("ln") else ""))
            return UNIT
        if name == "Some":
            return some(self.value(args[0]))
        if name == "Ok":
            return ok(self.value(args[0]) if args else UNIT)
        if name == "Err":
            return err(self.value(args[0]))
        if name == "panic":
            for a in args:
                self.read(a)
            raise KPanic("panic")
        if name in ("assert", "debug_assert"):
            if not self.read(args[0]).v:
                raise KPanic("assertion failed")
            return UNIT
        if name in ("assert_eq", "assert_ne"):
            r = self.equal(self.read(args[0]), self.read(args[1]))
            if r != (name == "assert_eq"):
                raise KPanic("assertion failed")
            return UNIT
        if name == "drop":
            self.drop_value(self.value(args[0]))
            return UNIT
        if name in ("min", "max"):
            a, b = self.read(args[0]), self.read(args[1])
            return a if (self.sort_key(a) <= self.sort_key(b)) == (name == "min") else b
        raise Unsupported(f"builtin {name}")

    def builtin_static(self, t, m, args):
        if (t, m) in (("Vec", "new"), ("Vec", "with_capacity"), ("Array", "new")):
            for a in args:
                if self.read(a).v < 0:
                    raise KPanic("negative capacity")
            return VecV([])
        if t in ("Map", "Set", "SortedMap", "SortedSet", "HashMap", "HashSet") and m in ("new", "with_capacity"):
            for a in args:
                self.read(a)
            kind = {"HashMap": "Map", "HashSet": "Set"}.get(t, t)
            return MapV([], [], kind)
        if t == "String" and m in ("new", "with_capacity"):
            return Str("")
        if t == "String" and m == "from":
            return Str(self.fmt(self.read(args[0]), ""))
        if t == "mem" and m == "take":
            r = self.borrow(args[0], True)
            old = self.get(r.cell, r.path)
            self.set_raw(r.cell, r.path, self.default_like(old))
            return old
        if t == "mem" and m == "replace":
            r = self.borrow(args[0], True)
            nv = self.value(args[1])
            old = self.get(r.cell, r.path)
            self.set_raw(r.cell, r.path, nv)
            return old
        if t == "mem" and m == "swap":
            a = self.borrow(args[0], True)
            b = self.borrow(args[1], True)
            x, y = self.get(a.cell, a.path), self.get(b.cell, b.path)
            self.set_raw(a.cell, a.path, y)
            self.set_raw(b.cell, b.path, x)
            return UNIT
        if t in INT_TYPES and m in ("MAX", "MIN"):
            raise Unsupported("int consts")
        raise Unsupported(f"static {t}.{m}")

    def default_like(self, v):
        if isinstance(v, VecV):
            return VecV([], v.kind)
        if isinstance(v, Str):
            return Str("")
        if isinstance(v, Enum) and v.ty == "Option":
            return none()
        if isinstance(v, MapV):
            return MapV([], [], v.kind)
        if isinstance(v, Prim):
            return Prim(type(v.v)(), v.kind)
        raise Unsupported("mem.take of a type without a known Default")

    def e_mcall(self, e):
        _, recv, name, args = e
        if recv[0] == "path" or (recv[0] == "var" and recv[1][:1].isupper() and recv[1] in self.p.structs):
            raise Unsupported("method on a path")
        c, p, b = self.resolve_or_temp(recv)
        v0 = self.get(c, p)
        v = self.deref_value(v0)
        tname = self.type_name(v0 if not isinstance(v0, Ref) else v)
        if isinstance(v0, Ref):
            tname = self.type_name(v)
        f = self.find_method(tname, name)
        if f is not None and f.recv is not None:
            if f.recv == "own":
                if isinstance(v0, Ref):
                    r = self.read_through_ref(self.deref_refs(v0))
                    if r is None:
                        raise ModelError("move out of a borrowed receiver (C3)")
                else:
                    r = self.use(c, p, b)
            else:
                r = v0 if isinstance(v0, Ref) else Ref(c, p, f.recv == "mutref")
            return self.call_user(f, r, self.args_for(f, args), tname)
        return self.builtin_method(c, p, b, v0, v, name, args)

    def builtin_method(self, c, p, b, v0, v, name, args):
        # receiver place after following references and handles
        rc, rp, rb = self.deref(c, p, b)
        self._recv_ty = self.declared_type(c, p) or self.declared_type(rc, rp)
        if name == "clone" and not args:
            return self.clone_value(v0 if isinstance(v0, Handle) else v)
        if name == "to_string" and not args:
            return Str(self.fmt(v, ""))
        if isinstance(v, Prim):
            return self.prim_method(v, name, args)
        if isinstance(v, Str):
            return self.str_method(rc, rp, rb, v, name, args)
        if isinstance(v, VecV):
            return self.vec_method(rc, rp, rb, v, name, args)
        if isinstance(v, Enum) and v.ty in ("Option", "Result"):
            return self.optres_method(c, p, b, rc, rp, rb, v0, v, name, args)
        if isinstance(v, MapV):
            return self.map_method(rc, rp, rb, v, name, args)
        if isinstance(v, Tup) and name == "len":
            return Prim(len(v.elems), "int")
        raise Unsupported(f"method {self.type_name(v)}.{name}")

    def prim_method(self, v, name, args):
        x = v.v
        if name == "abs":
            if v.kind == "int" and abs(x) >= 2 ** 63:
                raise KPanic("integer overflow")
            return Prim(abs(x), v.kind)
        if name in ("min", "max"):
            y = self.read(args[0]).v
            return Prim(min(x, y) if name == "min" else max(x, y), v.kind)
        if name == "pow":
            return Prim(x ** self.read(args[0]).v, v.kind)
        if name == "is_some":
            raise Unsupported("is_some on prim")
        if name == "cmp":
            y = self.read(args[0]).v
            return Enum("Ordering", "Less" if x < y else "Greater" if x > y else "Equal", None)
        if name in ("sqrt",):
            return Prim(x ** 0.5, "float")
        raise Unsupported(f"method {v.kind}.{name}")

    def str_method(self, c, p, b, v, name, args):
        if name == "len":
            return Prim(len(v.s.encode()), "int")
        if name == "is_empty":
            return Prim(not v.s, "bool")
        if name in ("push_str", "push"):
            if b == "ref" and False:
                pass
            add = self.fmt(self.read(args[0]), "")
            self.set_raw(c, p, Str(v.s + add))
            return UNIT
        if name in ("as_str", "to_owned", "trim", "to_uppercase", "to_lowercase"):
            s = v.s
            if name == "trim":
                s = s.strip()
            elif name == "to_uppercase":
                s = s.upper()
            elif name == "to_lowercase":
                s = s.lower()
            return Str(s)
        if name in ("contains", "starts_with", "ends_with"):
            a = self.read(args[0])
            t = a.s if isinstance(a, Str) else a.v
            return Prim({"contains": t in v.s, "starts_with": v.s.startswith(t), "ends_with": v.s.endswith(t)}[name], "bool")
        if name == "eq":
            return Prim(self.equal(v, self.read(args[0])), "bool")
        raise Unsupported(f"method String.{name}")

    def vec_method(self, c, p, b, v, name, args):
        if name == "len":
            return Prim(len(v.elems), "int")
        if name == "is_empty":
            return Prim(not v.elems, "bool")
        if name == "push":
            x = self.value(args[0])
            et = self.elem_types(v.kind)
            if et:
                x = self.store_owned(x, et[0], "collection element")
            v = self.get(c, p)
            v.elems.append(x)
            return UNIT
        if name == "pop":
            if not v.elems:
                return none()
            return some(v.elems.pop())
        if name in ("get", "first", "last"):
            if name == "get":
                i = self.read(args[0]).v
            elif args:
                if name == "first":
                    raise Unsupported("Vec.first with an argument")
                i = len(v.elems) - 1 - self.read(args[0]).v  # `last(k)` counts back from the end
            else:
                i = 0 if name == "first" else len(v.elems) - 1
            if not 0 <= i < len(v.elems):
                return none()
            return some(Ref(c, p + [("i", i)]))
        if name == "clear":
            old = v.elems[:]
            v.elems.clear()
            for x in old:
                self.drop_value(x)
            return UNIT
        if name == "truncate":
            n = self.read(args[0]).v
            old = v.elems[n:]
            del v.elems[n:]
            for x in old:
                self.drop_value(x)
            return UNIT
        if name == "insert":
            i = self.read(args[0]).v
            x = self.value(args[1])
            et = self.elem_types(v.kind)
            if et:
                x = self.store_owned(x, et[0], "collection element")
            v.elems.insert(i, x)
            return UNIT
        if name in ("remove", "swap_remove"):
            i = self.read(args[0]).v
            if not 0 <= i < len(v.elems):
                raise KPanic("index out of bounds")
            if name == "remove":
                return v.elems.pop(i)
            x = v.elems[i]
            last = v.elems.pop()
            if i < len(v.elems):
                v.elems[i] = last
            return x
        if name == "contains":
            y = self.read(args[0])
            return Prim(any(self.equal(x, y) for x in v.elems), "bool")
        if name == "swap":
            i, j = self.read(args[0]).v, self.read(args[1]).v
            v.elems[i], v.elems[j] = v.elems[j], v.elems[i]
            return UNIT
        if name == "reverse":
            v.elems.reverse()
            return UNIT
        if name == "sort":
            v.elems.sort(key=self.sort_key)
            return UNIT
        if name == "extend" or name == "append":
            o = self.value(args[0]) if name == "extend" else self.borrow(args[0], True)
            if isinstance(o, Ref):
                src = self.get(o.cell, o.path)
                v.elems.extend(src.elems)
                src.elems.clear()
            else:
                o = self.deref_value(o)
                v.elems.extend(o.elems)
            return UNIT
        if name == "join":
            sep = self.read(args[0]).s
            return Str(sep.join(self.fmt(x, "") for x in v.elems))
        if name == "sum":
            return Prim(sum(self.deref_value(x).v for x in v.elems), "int")
        raise Unsupported(f"method Vec.{name}")

    def optres_method(self, c, p, b, rc, rp, rb, v0, v, name, args):
        is_opt = v.ty == "Option"
        good = v.var in ("Some", "Ok")
        if name in ("is_some", "is_ok"):
            return Prim(good, "bool")
        if name in ("is_none", "is_err"):
            return Prim(not good, "bool")
        if name in ("unwrap", "expect", "unwrap_or", "unwrap_or_default", "unwrap_err"):
            for a in args[:1] if name == "expect" else []:
                self.read(a)
            want_good = name != "unwrap_err"
            whole = self.use(c, p, b) if not isinstance(v0, Ref) else None
            if whole is None:
                whole = self.read_through_ref(self.deref_refs(v0))
                if whole is None:
                    raise ModelError("move out of a borrowed Option/Result (C3)")
            if (whole.var in ("Some", "Ok")) == want_good:
                x = whole.payload[0]
                if name == "unwrap_or":
                    self.drop_value(self.value(args[0]))
                return x
            if name == "unwrap_or":
                self.drop_value(whole)
                return self.value(args[0])
            raise KPanic(f"{name} on {whole.var}")
        if name == "take" and is_opt:
            old = self.get(rc, rp)
            self.set_raw(rc, rp, none())
            return old
        if name == "replace" and is_opt:
            nv = some(self.value(args[0]))
            old = self.get(rc, rp)
            self.set_raw(rc, rp, nv)
            return old
        if name in ("as_ref", "as_mut") and is_opt:
            if not good:
                return none()
            return some(Ref(rc, rp + [("i", 0)]))
        raise Unsupported(f"method {v.ty}.{name}")

    def map_find(self, m, k):
        for i, x in enumerate(m.keys):
            if x is not MOVED and self.equal(x, k):
                return i
        return None

    def map_method(self, c, p, b, m, name, args):
        if name == "len":
            return Prim(len(m.keys), "int")
        if name == "is_empty":
            return Prim(not m.keys, "bool")
        if name == "insert":
            k = self.value(args[0])
            et = self.elem_types(m.kind)
            if et:
                k = self.store_owned(k, et[0], "collection key")
            if m.kind in ("Set", "SortedSet"):
                i = self.map_find(m, k)
                if i is not None:
                    self.drop_value(k)
                    return Prim(False, "bool")
                self.map_add(m, k, None)
                return Prim(True, "bool")
            v = self.value(args[1])
            if et and len(et) > 1:
                v = self.store_owned(v, et[1], "collection value")
            i = self.map_find(m, k)
            if i is not None:
                old = m.vals[i]
                m.vals[i] = v
                self.drop_value(k)
                return some(old)
            self.map_add(m, k, v)
            return none()
        if name in ("get", "get_mut"):
            k = self.read(args[0])
            i = self.map_find(m, k)
            if i is None:
                return none()
            return some(Ref(c, p + [("i", i)]))
        if name in ("contains_key", "contains"):
            return Prim(self.map_find(m, self.read(args[0])) is not None, "bool")
        if name == "remove":
            i = self.map_find(m, self.read(args[0]))
            if i is None:
                return none() if m.kind in ("Map", "SortedMap") else Prim(False, "bool")
            k = m.keys.pop(i)
            v = m.vals.pop(i)
            self.drop_value(k)
            if m.kind in ("Set", "SortedSet"):
                return Prim(True, "bool")
            return some(v)
        raise Unsupported(f"method Map.{name}")


# ───────────────────────────── entry point ─────────────────────────────


_WIDE = re.compile(r"\b(?:i128|u128|u64|usize)\b|\b\d+_?(?:u64|i128|u128)\b")
_WEAK = re.compile(r"\bweak\s+[A-Z]|\bWeak\[|\.downgrade\(")
_NARROW = re.compile(r"\b(?:i8|i16|i32|u8|u16|u32)\b")


def run_source(src: str):
    if _WEAK.search(src):
        raise Unsupported("weak references")
    prog = parse(src)
    m = Model(prog)
    if "main" not in prog.fns:
        raise Unsupported("no main")
    try:
        r = m.call_user(prog.fns["main"], None, [], None)
        code = 1 if isinstance(r, Enum) and r.ty == "Result" and r.var == "Err" else 0  # §10.3
    except KPanic as e:
        if str(e) == "integer overflow" and _WIDE.search(src):
            raise Unsupported("integer width (the model's integers are i64)")
        m.flags.add(f"panic: {e}")
        code = 101
    except RecursionError:
        raise Unsupported("recursion depth")
    if _NARROW.search(src):
        m.flags.add("narrow-int")
    return "".join(m.out), code, m.flags


if __name__ == "__main__":
    out, code, flags = run_source(open(sys.argv[1]).read())
    sys.stdout.write(out)
    print(f"[exit {code}] {sorted(flags)}", file=sys.stderr)
