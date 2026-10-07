"""A parser for the subset of Kāra that the corpus's drop programs use.

It exists so that the drop reference model can run corpus programs written as
ordinary Kāra source, not only the programs it generates itself. It shares no
code with the compiler: it is a second, independent reading of the syntax.

Anything outside the subset raises Unsupported, and the caller reports the
program as skipped, never as passing.
"""
from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Optional


class Unsupported(Exception):
    pass


class ParseError(Exception):
    pass


# ───────────────────────────── lexer ─────────────────────────────


@dataclass
class Tok:
    k: str  # id | int | float | str | fstr | char | op | eof
    v: object
    nl: bool  # a newline precedes this token
    line: int


OPS = [
    "..=", "...", "<<=", ">>=",
    "=>", "->", "::", "==", "!=", "<=", ">=", "&&", "||", "..", "+=", "-=", "*=", "/=", "%=",
    "<<", ">>", "?.", "|=", "&=", "^=",
    "+", "-", "*", "/", "%", "=", "<", ">", "!", "&", "|", "^", ".", ",", ";", ":", "(", ")",
    "[", "]", "{", "}", "?", "@", "#",
]

_ESC = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", "\\": "\\", '"': '"', "'": "'", "{": "{", "}": "}"}


def _unescape(s: str) -> str:
    out = []
    i = 0
    while i < len(s):
        c = s[i]
        if c == "\\" and i + 1 < len(s):
            n = s[i + 1]
            if n == "u" and i + 2 < len(s) and s[i + 2] == "{":
                j = s.index("}", i)
                out.append(chr(int(s[i + 3:j], 16)))
                i = j + 1
                continue
            out.append(_ESC.get(n, n))
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def lex(src: str) -> list:
    toks = []
    i = 0
    n = len(src)
    line = 1
    nl = True
    while i < n:
        c = src[i]
        if c == "\n":
            line += 1
            nl = True
            i += 1
            continue
        if c in " \t\r":
            i += 1
            continue
        if src.startswith("//", i):
            while i < n and src[i] != "\n":
                i += 1
            continue
        if src.startswith("/*", i):
            depth = 0
            while i < n:
                if src.startswith("/*", i):
                    depth += 1
                    i += 2
                elif src.startswith("*/", i):
                    depth -= 1
                    i += 2
                    if depth == 0:
                        break
                else:
                    if src[i] == "\n":
                        line += 1
                        nl = True
                    i += 1
            continue
        start_line = line
        if c == "f" and i + 1 < n and src[i + 1] == '"':
            j = i + 2
            depth = 0
            while j < n:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == "{":
                    if j + 1 < n and src[j + 1] == "{" and depth == 0:
                        j += 2
                        continue
                    depth += 1
                elif src[j] == "}":
                    if depth == 0 and j + 1 < n and src[j + 1] == "}":
                        j += 2
                        continue
                    depth -= 1
                elif src[j] == '"' and depth == 0:
                    break
                elif src[j] == "\n":
                    line += 1
                j += 1
            toks.append(Tok("fstr", src[i + 2:j], nl, start_line))
            i = j + 1
            nl = False
            continue
        if c == "r" and src.startswith('r"', i) or src.startswith('r#"', i):
            hashes = 0
            j = i + 1
            while src[j] == "#":
                hashes += 1
                j += 1
            end = '"' + "#" * hashes
            k = src.index(end, j + 1)
            body = src[j + 1:k]
            line += body.count("\n")
            toks.append(Tok("str", body, nl, start_line))
            i = k + len(end)
            nl = False
            continue
        if c == '"':
            j = i + 1
            while True:
                if j >= len(src):
                    raise ParseError(f"line {line}: unterminated string")
                if src[j] == '"':
                    break
                if src[j] == "\\":
                    j += 1
                elif src[j] == "\n":
                    line += 1
                j += 1
            toks.append(Tok("str", _unescape(src[i + 1:j]), nl, start_line))
            i = j + 1
            nl = False
            continue
        if c == "'" :
            m = re.match(r"'(\\u\{[0-9a-fA-F]+\}|\\.|[^\\'])'", src[i:])
            if m:
                toks.append(Tok("char", _unescape(m.group(1)), nl, line))
                i += m.end()
                nl = False
                continue
            raise Unsupported("lifetime or odd quote")
        if c.isdigit():
            m = re.match(r"0x[0-9a-fA-F_]+|0b[01_]+|0o[0-7_]+|\d[\d_]*(\.\d[\d_]*)?([eE][+-]?\d+)?", src[i:])
            s = m.group(0)
            j = i + len(s)
            # `1..2` must not lex as a float
            if "." in s and src.startswith(".", i + len(s.split(".")[0]) + 1) and False:
                pass
            if m.group(1) and src[i + len(s.split(".")[0]):].startswith(".."):
                s = s.split(".")[0]
                j = i + len(s)
            suf = re.match(r"(i8|i16|i32|i64|i128|isize|u8|u16|u32|u64|u128|usize|f32|f64)", src[j:])
            if suf:
                j += suf.end()
            t = s.replace("_", "")
            if t.startswith(("0x", "0b", "0o")):
                toks.append(Tok("int", int(t, 0), nl, line))
            elif "." in t or "e" in t or "E" in t or (suf and suf.group(1).startswith("f")):
                toks.append(Tok("float", float(t), nl, line))
            else:
                toks.append(Tok("int", int(t), nl, line))
            i = j
            nl = False
            continue
        if c.isalpha() or c == "_":
            m = re.match(r"[A-Za-z_][A-Za-z0-9_]*", src[i:])
            word = m.group(0)
            if word == "vec" and src.startswith("vec!", i):
                toks.append(Tok("id", "vec!", nl, line))
                i += 4
                nl = False
                continue
            toks.append(Tok("id", word, nl, line))
            i += m.end()
            nl = False
            continue
        for op in OPS:
            if src.startswith(op, i):
                toks.append(Tok("op", op, nl, line))
                i += len(op)
                nl = False
                break
        else:
            raise ParseError(f"line {line}: unexpected {c!r}")
    toks.append(Tok("eof", None, True, line))
    return toks


# ───────────────────────────── AST ─────────────────────────────
# Expressions are tuples tagged by their first element, which keeps the
# interpreter's dispatch a dict lookup. Statements likewise.
#
#   ("int", n) ("float", x) ("bool", b) ("str", s) ("char", c) ("unit",)
#   ("fstr", [str | (expr, spec)])
#   ("var", name)
#   ("path", [names])                    E.A, Type.method, Option.None
#   ("field", base, name) ("tidx", base, i) ("index", base, idx)
#   ("call", callee, [args])             callee is an expr
#   ("mcall", recv, name, [args])
#   ("struct", tyname, [(fname, expr)], base|None)   tyname may be "E.V"
#   ("tuple", [exprs]) ("array", [exprs]) ("arrayrep", e, n)
#   ("bin", op, a, b) ("un", op, a) ("cast", e, ty)
#   ("range", lo, hi, inclusive)
#   ("if", cond, block, else_block|None)  cond may be ("let", pat, expr)
#   ("match", scrut, [(pat, guard, expr)])
#   ("block", Block) ("loop", Block) ("while", cond, Block) ("for", pat, expr, Block)
#   ("closure", [params], body_expr, is_move)
#   ("try", e)  ("ret", e|None) ("break", e|None) ("continue",)
#   ("assign", op, place, expr)
#
# Patterns:
#   ("pwild",) ("pbind", name, by_ref, mut) ("plit", value) ("prange", lo, hi)
#   ("ptuple", [pats]) ("penum", path, [pats]|None) ("pstruct", path, [(f, pat)], rest)
#   ("por", [pats]) ("pat_at", name, pat)


@dataclass
class Block:
    stmts: list
    tail: object = None  # expr or None


@dataclass
class Param:
    name: str
    mode: str  # own | ref | mutref
    ty: object
    is_mut: bool = False


@dataclass
class FnDef:
    name: str
    params: list
    ret: object
    body: Optional[Block]
    recv: Optional[str] = None  # None (static) | own | ref | mutref
    generics: list = field(default_factory=list)


@dataclass
class StructDef:
    name: str
    fields: list  # [(name, ty)]
    shared: bool = False
    derives: set = field(default_factory=set)


@dataclass
class EnumDef:
    name: str
    variants: dict  # vname -> None | [ty] | [(fname, ty)] (struct variant: list of tuples)
    order: list = field(default_factory=list)
    shared: bool = False
    derives: set = field(default_factory=set)
    struct_variants: set = field(default_factory=set)


@dataclass
class Program:
    fns: dict
    structs: dict
    enums: dict
    methods: dict  # type name -> {method: FnDef}
    drops: set  # type names with a Drop body
    traits: dict
    consts: dict
    trait_impls: dict = field(default_factory=dict)  # type -> set(trait)
    from_impls: dict = field(default_factory=dict)  # (target, source) -> FnDef


KEYWORDS = {"let", "mut", "fn", "if", "else", "match", "while", "for", "in", "loop", "return",
            "break", "continue", "struct", "enum", "impl", "trait", "ref", "true", "false",
            "defer", "errdefer", "as", "pub", "const", "shared", "move", "self", "use", "import"}


class Parser:
    def __init__(self, src: str):
        self.t = lex(src)
        self.i = 0
        self.no_struct = 0
        self.fn_names = set()
        for j, tok in enumerate(self.t):
            if tok.k == "id" and tok.v == "fn" and j + 1 < len(self.t) and self.t[j + 1].k == "id":
                self.fn_names.add(self.t[j + 1].v)

    # ── token helpers ──
    @property
    def cur(self) -> Tok:
        return self.t[self.i]

    def peek(self, k=1) -> Tok:
        return self.t[min(self.i + k, len(self.t) - 1)]

    def at(self, v, k=0) -> bool:
        tok = self.peek(k) if k else self.cur
        return tok.k in ("op", "id") and tok.v == v

    def eat(self, v) -> bool:
        if self.at(v):
            self.i += 1
            return True
        return False

    def expect(self, v):
        if not self.eat(v):
            raise ParseError(f"line {self.cur.line}: expected {v!r}, got {self.cur.v!r}")

    def ident(self) -> str:
        tok = self.cur
        if tok.k != "id":
            raise ParseError(f"line {tok.line}: expected identifier, got {tok.v!r}")
        self.i += 1
        return tok.v

    # ── types (kept as nested tuples; only shape matters) ──
    def ty(self):
        if self.eat("ref"):
            return ("ref", self.ty())
        if self.at("mut") and self.at("ref", 1):
            self.i += 2
            return ("mutref", self.ty())
        if self.eat("("):
            elems = []
            while not self.eat(")"):
                elems.append(self.ty())
                self.eat(",")
            return ("tuple", elems)
        if self.at("["):
            self.i += 1
            inner = self.ty()
            if self.eat(";"):
                self.expr()
            self.expect("]")
            return ("app", "Array", [inner])
        if self.at("fn") or self.at("Fn") or self.at("escaping"):
            raise Unsupported("function types")
        if self.at("dyn"):
            raise Unsupported("dyn")
        name = self.ident()
        while self.at(".") and self.peek().k == "id" and self.peek().v[:1].isupper():
            self.i += 1
            name += "." + self.ident()
        args = []
        if self.at("["):
            self.i += 1
            while not self.eat("]"):
                if self.cur.k == "int":
                    self.i += 1
                else:
                    args.append(self.ty())
                self.eat(",")
        return ("app", name, args)

    def generics(self):
        gs = []
        if self.at("["):
            self.i += 1
            depth = 1
            while depth:
                if self.at("["):
                    depth += 1
                elif self.at("]"):
                    depth -= 1
                elif depth == 1 and self.cur.k == "id" and (self.peek(-1).v in ("[", ",")):
                    gs.append(self.cur.v)
                self.i += 1
        return gs

    def skip_where(self):
        if self.at("where"):
            while not self.at("{") and not self.at(";"):
                self.i += 1

    def skip_effects(self):
        # `with reads(X), writes(Y)` or `effects(...)` clauses between the signature and the body
        while self.cur.k == "id" and self.cur.v in ("with", "effects", "reads", "writes", "sends", "receives",
                                                    "allocates", "panics", "blocks", "suspends", "pure"):
            self.i += 1
            if self.at("("):
                depth = 0
                while True:
                    if self.at("("):
                        depth += 1
                    elif self.at(")"):
                        depth -= 1
                        if depth == 0:
                            self.i += 1
                            break
                    self.i += 1
            self.eat(",")

    # ── items ──
    def program(self) -> Program:
        prog = Program({}, {}, {}, {}, set(), {}, {})
        while self.cur.k != "eof":
            self.item(prog)
        return prog

    def attrs(self):
        derives = set()
        while self.at("#"):
            self.i += 1
            self.expect("[")
            depth = 1
            text = []
            while depth:
                if self.at("["):
                    depth += 1
                elif self.at("]"):
                    depth -= 1
                    if depth == 0:
                        self.i += 1
                        break
                text.append(str(self.cur.v))
                self.i += 1
            s = " ".join(text)
            if s.startswith("derive"):
                derives |= set(re.findall(r"[A-Z]\w*", s))
        return derives

    def item(self, prog: Program):
        derives = self.attrs()
        self.eat("pub")
        if self.at("use") or self.at("import") or self.at("mod"):
            raise Unsupported("modules")
        shared = False
        if self.at("shared") or self.at("par") or self.at("frozen"):
            if self.cur.v != "shared":
                raise Unsupported(f"{self.cur.v} types")
            shared = True
            self.i += 1
        if self.eat("struct"):
            name = self.ident()
            self.generics()
            self.skip_where()
            fields = []
            if self.eat("("):
                k = 0
                while not self.eat(")"):
                    self.eat("pub")
                    fields.append((str(k), self.ty()))
                    k += 1
                    self.eat(",")
                self.eat(";")
            elif self.eat("{"):
                while not self.eat("}"):
                    self.attrs()
                    self.eat("pub")
                    if self.eat("weak"):
                        raise Unsupported("weak fields")
                    self.eat("mut")
                    f = self.ident()
                    self.expect(":")
                    fields.append((f, self.ty()))
                    if self.eat("="):
                        raise Unsupported("field defaults")
                    self.eat(",")
            else:
                self.eat(";")
            if self.at("layout"):
                raise Unsupported("layout blocks")
            prog.structs[name] = StructDef(name, fields, shared, derives)
            return
        if self.eat("enum"):
            name = self.ident()
            self.generics()
            self.expect("{")
            ed = EnumDef(name, {}, [], shared, derives)
            while not self.eat("}"):
                self.attrs()
                v = self.ident()
                if self.eat("("):
                    tys = []
                    while not self.eat(")"):
                        tys.append(self.ty())
                        self.eat(",")
                    ed.variants[v] = tys
                elif self.eat("{"):
                    fs = []
                    while not self.eat("}"):
                        self.eat("mut")
                        f = self.ident()
                        self.expect(":")
                        fs.append((f, self.ty()))
                        self.eat(",")
                    ed.variants[v] = fs
                    ed.struct_variants.add(v)
                else:
                    if self.eat("="):
                        self.expr()
                    ed.variants[v] = None
                ed.order.append(v)
                self.eat(",")
            prog.enums[name] = ed
            return
        if self.eat("type"):
            raise Unsupported("type aliases")
        if self.eat("distinct"):
            raise Unsupported("distinct types")
        if self.at("let"):
            self.i += 1
            self.eat("mut")
            name = self.ident()
            if self.eat(":"):
                self.ty()
            self.expect("=")
            prog.consts[name] = self.expr()
            self.eat(";")
            return
        if self.eat("const") or self.eat("static"):
            name = self.ident()
            if self.eat(":"):
                self.ty()
            self.expect("=")
            prog.consts[name] = self.expr()
            self.eat(";")
            return
        if self.eat("trait"):
            name = self.ident()
            self.generics()
            if self.eat(":"):
                while not self.at("{"):
                    self.i += 1
            self.expect("{")
            methods = {}
            while not self.eat("}"):
                self.attrs()
                if self.eat("type"):
                    raise Unsupported("associated types")
                f = self.fn(require_body=False)
                methods[f.name] = f
            prog.traits[name] = methods
            return
        if self.eat("impl"):
            self.generics()
            t1 = self.ty()
            trait = None
            if self.eat("for"):
                trait = t1[1]
                target = self.ty()
            else:
                target = t1
            self.skip_where()
            tname = target[1] if target[0] == "app" else None
            if tname is None:
                raise Unsupported("impl on a non-nominal type")
            self.expect("{")
            ms = prog.methods.setdefault(tname, {})
            while not self.eat("}"):
                self.attrs()
                self.eat("pub")
                if self.at("type"):
                    raise Unsupported("associated types")
                f = self.fn()
                if trait == "Drop":
                    prog.drops.add(tname)
                    ms["__drop__"] = f
                elif trait == "From" and t1[2] and t1[2][0][0] == "app":
                    prog.from_impls[(tname, t1[2][0][1])] = f
                else:
                    if f.name in ms and ms[f.name] is not prog.traits.get(trait, {}).get(f.name) \
                            and not getattr(ms[f.name], "is_default", False):
                        # `impl Show for Vec[i64]` beside `impl Show for Vec[String]`: dispatch needs the type arguments
                        raise Unsupported(f"two impls define {tname}.{f.name} (dispatch per instantiation)")
                    ms[f.name] = f
            if trait and trait != "Drop":
                prog.trait_impls.setdefault(tname, set()).add(trait)
                # trait default methods the impl did not override
                for mname, mdef in prog.traits.get(trait, {}).items():
                    if mname not in ms and mdef.body is not None:
                        mdef.is_default = True
                        ms[mname] = mdef
            return
        if self.at("fn") or self.at("async") or self.at("extern") or self.at("unsafe"):
            if not self.at("fn"):
                raise Unsupported(self.cur.v)
            f = self.fn()
            prog.fns[f.name] = f
            return
        if self.at("test") or self.at("bench"):
            raise Unsupported("test blocks")
        raise ParseError(f"line {self.cur.line}: unexpected item token {self.cur.v!r}")

    def fn(self, require_body=True) -> FnDef:
        self.eat("pub")
        self.expect("fn")
        name = self.ident()
        gens = self.generics()
        self.expect("(")
        params = []
        recv = None
        while not self.eat(")"):
            self.attrs()
            if self.at("self"):
                self.i += 1
                recv = "own"
            elif self.at("ref") and self.at("self", 1):
                self.i += 2
                recv = "ref"
            elif self.at("mut") and self.at("ref", 1) and self.at("self", 2):
                self.i += 3
                recv = "mutref"
            elif self.at("mut") and self.at("self", 1):
                self.i += 2
                recv = "own"
            else:
                is_mut = self.eat("mut")
                if self.cur.k != "id":
                    raise Unsupported("pattern parameters")
                pname = self.ident()
                self.expect(":")
                if self.at("escaping") or self.at("Fn") or self.at("fn"):
                    raise Unsupported("function-typed parameters")
                t = self.ty()
                mode = "own"
                if t[0] == "ref":
                    mode, t = "ref", t[1]
                elif t[0] == "mutref":
                    mode, t = "mutref", t[1]
                elif t[0] == "app" and t[1] in ("Slice", "StringSlice"):
                    mode = "ref"
                params.append(Param(pname, mode, t, is_mut))
            if self.at(":") and recv is not None:
                self.i += 1
                self.ty()
            self.eat(",")
        ret = None
        if self.eat("->"):
            ret = self.ty()
        self.skip_effects()
        self.skip_where()
        self.skip_effects()
        body = None
        if self.at("{"):
            body = self.block()
        elif require_body:
            raise ParseError(f"line {self.cur.line}: fn {name} without a body")
        else:
            self.eat(";")
        return FnDef(name, params, ret, body, recv, gens)

    # ── statements ──
    def block(self) -> Block:
        self.expect("{")
        saved = self.no_struct
        self.no_struct = 0
        stmts = []
        tail = None
        while not self.at("}"):
            if self.eat(";"):
                continue
            s = self.stmt()
            if s[0] == "expr":
                e = s[1]
                if self.eat(";"):
                    stmts.append(("expr", e, True))
                elif self.at("}"):
                    tail = e
                else:
                    stmts.append(("expr", e, False))
            else:
                stmts.append(s)
                self.eat(";")
        self.expect("}")
        self.no_struct = saved
        return Block(stmts, tail)

    def stmt(self):
        if self.at("let"):
            self.i += 1
            pat = self.pattern(top=True)
            ty = None
            if self.eat(":"):
                ty = self.ty()
            init = None
            if self.eat("="):
                init = self.expr()
            if self.at("else"):
                self.i += 1
                return ("letelse", pat, ty, init, self.block())
            return ("let", pat, ty, init)
        if self.at("defer"):
            self.i += 1
            return ("defer", self.block_or_stmt())
        if self.at("errdefer"):
            self.i += 1
            return ("errdefer", self.block_or_stmt())
        if self.at("fn") or self.at("struct") or self.at("enum") or self.at("impl"):
            raise Unsupported("local items")
        if self.cur.k == "id" and self.cur.v in ("if", "match", "loop", "while", "for") or self.at("{"):
            e = self.primary()
            if not (self.at(".") and not self.cur.nl) and not self.at("?"):
                return ("expr", e, "blocklike")
            e = self.postfix(e)
        else:
            e = self.expr()
        if self.cur.k == "op" and self.cur.v in ("=", "+=", "-=", "*=", "/=", "%=", "|=", "&=", "^=", "<<=", ">>="):
            op = self.cur.v
            self.i += 1
            rhs = self.expr()
            return ("expr", ("assign", op, e, rhs))
        return ("expr", e)

    def block_or_stmt(self) -> Block:
        if self.at("{"):
            return self.block()
        s = self.stmt()
        if s[0] == "expr":
            return Block([("expr", s[1], True)])
        return Block([s])

    # ── patterns ──
    def pattern(self, top=False):
        p = self.pattern1()
        if self.at("|") and not top:
            ps = [p]
            while self.eat("|"):
                ps.append(self.pattern1())
            return ("por", ps)
        return p

    def pattern1(self):
        tok = self.cur
        if self.eat("("):
            ps = []
            while not self.eat(")"):
                ps.append(self.pattern())
                self.eat(",")
            return ps[0] if len(ps) == 1 else ("ptuple", ps)
        if self.eat("["):
            raise Unsupported("slice patterns")
        if tok.k == "op" and tok.v == "-":
            self.i += 1
            v = -self.cur.v
            self.i += 1
            return self.prange_tail(("plit", v))
        if tok.k in ("int", "str", "char", "float"):
            self.i += 1
            return self.prange_tail(("plit", tok.v))
        if self.eat("true"):
            return ("plit", True)
        if self.eat("false"):
            return ("plit", False)
        if self.eat(".."):
            return ("prest",)
        if tok.k == "id":
            if tok.v == "_":
                self.i += 1
                return ("pwild",)
            by_ref = False
            mut = False
            if self.at("ref"):
                self.i += 1
                by_ref = True
            if self.at("mut"):
                self.i += 1
                mut = True
                if self.at("ref"):
                    self.i += 1
                    by_ref = True
            name = self.ident()
            path = [name]
            while self.at(".") and self.peek().k == "id":
                self.i += 1
                path.append(self.ident())
            if self.at("["):  # generic args on a path pattern
                self.generics()
            if self.eat("@"):
                return ("pat_at", name, self.pattern1())
            if self.eat("("):
                ps = []
                while not self.eat(")"):
                    ps.append(self.pattern())
                    self.eat(",")
                return ("penum", path, ps)
            if self.at("{") and (len(path) > 1 or path[0][:1].isupper()):
                self.i += 1
                fs = []
                rest = False
                while not self.eat("}"):
                    if self.eat(".."):
                        rest = True
                        continue
                    r = self.eat("ref")
                    m = self.eat("mut")
                    f = self.ident()
                    if self.eat(":"):
                        fs.append((f, self.pattern()))
                    else:
                        fs.append((f, ("pbind", f, r, m)))
                    self.eat(",")
                return ("pstruct", path, fs, rest)
            if len(path) > 1 or (name[:1].isupper() and not by_ref and not mut):
                return ("penum", path, None)
            if name in ("None",):
                return ("penum", path, None)
            return ("pbind", name, by_ref, mut)
        raise ParseError(f"line {tok.line}: bad pattern token {tok.v!r}")

    def prange_tail(self, p):
        if self.at("..=") or self.at(".."):
            exclusive = self.at("..")
            self.i += 1
            neg = self.eat("-")
            hi = self.cur.v
            self.i += 1
            hi = -hi if neg else hi
            if exclusive:
                if not isinstance(hi, int):
                    raise Unsupported("exclusive range pattern over a non-integer")
                hi -= 1
            return ("prange", p[1], hi)
        return p

    # ── expressions ──
    BIN = [
        ["||"], ["&&"], ["==", "!=", "<", ">", "<=", ">="], ["|"], ["^"], ["&"], ["<<", ">>"],
        ["+", "-"], ["*", "/", "%"],
    ]

    def expr(self):
        if self.at("return"):
            self.i += 1
            if self.at("}") or self.at(";") or self.at(",") or self.cur.nl or self.at(")"):
                return ("ret", None)
            return ("ret", self.expr())
        if self.at("break"):
            self.i += 1
            if self.at("}") or self.at(";") or self.at(",") or self.cur.nl:
                return ("break", None)
            return ("break", self.expr())
        if self.at("continue"):
            self.i += 1
            return ("continue",)
        return self.range_expr()

    def range_expr(self):
        if self.at("..") or self.at("..="):
            raise Unsupported("open ranges")
        lo = self.binary(0)
        if (self.at("..") or self.at("..=")) and not self.cur.nl:
            inc = self.cur.v == "..="
            self.i += 1
            if self.at("]") or self.at(")") or self.at("{") and self.no_struct:
                return ("range", lo, None, inc)
            hi = self.binary(0)
            return ("range", lo, hi, inc)
        return lo

    def binary(self, lvl):
        if lvl == len(self.BIN):
            return self.unary()
        a = self.binary(lvl + 1)
        while (self.cur.k == "op" and self.cur.v in self.BIN[lvl]) or (
                self.cur.k == "id" and self.cur.v in ("and", "or") and self.BIN[lvl][0] == ("&&" if self.cur.v == "and" else "||")):
            if self.cur.nl and self.cur.v not in ("&&", "||", "and", "or"):
                break
            op = {"and": "&&", "or": "||"}.get(self.cur.v, self.cur.v)
            self.i += 1
            b = self.binary(lvl + 1)
            a = ("bin", op, a, b)
        return a

    def prefix(self):
        if self.at("-"):
            self.i += 1
            e = self.prefix()
            if e[0] in ("int", "float"):
                return (e[0], -e[1])
            return ("un", "-", e)
        if self.at("!") or self.at("not"):
            self.i += 1
            return ("un", "!", self.prefix())
        if self.at("&") or self.at("*"):
            raise Unsupported("& / * operators")
        return self.postfix()

    def unary(self):
        # prefix operators bind tighter than `as`: `-1i32 as u8` is `(-1i32) as u8`
        e = self.prefix()
        while self.at("as"):
            self.i += 1
            e = ("cast", e, self.ty())
        return e

    def args(self):
        self.expect("(")
        saved = self.no_struct
        self.no_struct = 0
        out = []
        while not self.eat(")"):
            if self.at("mut") and not self.at("ref", 1):
                self.i += 1  # call-site mutation marker
            if self.cur.k == "id" and self.peek().k == "op" and self.peek().v == ":" :
                self.i += 2  # named argument
            out.append(self.expr())
            self.eat(",")
        self.no_struct = saved
        return out

    def postfix(self, start=None):
        e = self.primary() if start is None else start
        while True:
            if self.at("(") and not self.cur.nl:
                e = ("call", e, self.args())
            elif self.at(".") or (self.at("?.")):
                if self.at("?."):
                    raise Unsupported("?. chains")
                self.i += 1
                if self.cur.k == "int":
                    e = ("tidx", e, self.cur.v)
                    self.i += 1
                    continue
                if self.cur.k == "float":
                    # `t.0.1` lexes as t . 0.1
                    a, b = str(self.cur.v).split(".")
                    e = ("tidx", ("tidx", e, int(a)), int(b))
                    self.i += 1
                    continue
                name = self.ident()
                if self.at("[") and self.is_generic_args(("(",)):
                    self.generics()
                if self.at("(") and not self.cur.nl:
                    e = ("mcall", e, name, self.args())
                else:
                    e = ("field", e, name)
            elif self.at("[") and not self.cur.nl:
                self.i += 1
                saved = self.no_struct
                self.no_struct = 0
                idx = self.expr()
                self.no_struct = saved
                self.expect("]")
                e = ("index", e, idx)
            elif self.at("?") and not self.cur.nl:
                self.i += 1
                e = ("try", e)
            else:
                return e

    def is_generic_args(self, follow=("(", "{", ".")) -> bool:
        # `[...]` followed by `(` `{` or `.`: type arguments, not an index
        depth = 0
        j = self.i
        while j < len(self.t):
            tok = self.t[j]
            if tok.k == "op" and tok.v == "[":
                depth += 1
            elif tok.k == "op" and tok.v == "]":
                depth -= 1
                if depth == 0:
                    nxt = self.t[j + 1]
                    return nxt.k == "op" and nxt.v in follow and not nxt.nl
            elif tok.k not in ("id",) and not (tok.k == "op" and tok.v in (",", "[", "]")) and tok.k != "int":
                return False
            j += 1
        return False

    def primary(self):
        tok = self.cur
        if tok.k == "int":
            self.i += 1
            return ("int", tok.v)
        if tok.k == "float":
            self.i += 1
            return ("float", tok.v)
        if tok.k == "str":
            self.i += 1
            return ("str", tok.v)
        if tok.k == "char":
            self.i += 1
            return ("char", tok.v)
        if tok.k == "fstr":
            self.i += 1
            return ("fstr", self.fstring(tok.v, tok.line))
        if tok.k == "op":
            if tok.v == "(":
                self.i += 1
                saved = self.no_struct
                self.no_struct = 0
                if self.eat(")"):
                    self.no_struct = saved
                    return ("unit",)
                es = [self.expr()]
                tup = False
                while self.eat(","):
                    tup = True
                    if self.at(")"):
                        break
                    es.append(self.expr())
                self.expect(")")
                self.no_struct = saved
                return ("tuple", es) if tup else es[0]
            if tok.v == "[":
                self.i += 1
                saved = self.no_struct
                self.no_struct = 0
                es = []
                while not self.eat("]"):
                    es.append(self.expr())
                    if self.eat(";"):
                        n = self.expr()
                        self.expect("]")
                        self.no_struct = saved
                        return ("arrayrep", es[0], n)
                    self.eat(",")
                self.no_struct = saved
                return ("array", es)
            if tok.v == "{":
                return ("block", self.block())
            if tok.v in ("|", "||"):
                return self.closure(False)
            raise ParseError(f"line {tok.line}: unexpected {tok.v!r}")
        if tok.k != "id":
            raise ParseError(f"line {tok.line}: unexpected token {tok.v!r}")
        w = tok.v
        if w == "true" or w == "false":
            self.i += 1
            return ("bool", w == "true")
        if w == "move" and (self.at("|", 1) or self.at("||", 1)):
            self.i += 1
            return self.closure(True)
        if w == "if":
            return self.if_expr()
        if w == "match":
            self.i += 1
            self.no_struct += 1
            scrut = self.expr()
            self.no_struct -= 1
            self.expect("{")
            saved = self.no_struct
            self.no_struct = 0
            arms = []
            while not self.eat("}"):
                pat = self.pattern()
                guard = None
                if self.eat("if"):
                    guard = self.expr()
                self.expect("=>")
                body = self.expr()
                arms.append((pat, guard, body))
                self.eat(",")
            self.no_struct = saved
            return ("match", scrut, arms)
        if w == "loop":
            self.i += 1
            return ("loop", self.block())
        if w == "while":
            self.i += 1
            cond = self.cond()
            return ("while", cond, self.block())
        if w == "for":
            self.i += 1
            pat = self.pattern(top=True)
            self.expect("in")
            self.no_struct += 1
            it = self.expr()
            self.no_struct -= 1
            return ("for", pat, it, self.block())
        if w == "unsafe" or w == "comptime" or w == "par" or w == "async" or w == "await":
            raise Unsupported(w)
        if w == "Vec" and self.at("[", 1):
            self.i += 1
            if self.is_generic_args():
                self.i -= 1
            else:
                self.i -= 1
                w = "vec!"
        if w == "vec!":
            self.i += 1
            self.expect("[")
            es = []
            while not self.eat("]"):
                es.append(self.expr())
                if self.eat(";"):
                    n = self.expr()
                    self.expect("]")
                    return ("arrayrep", es[0], n)
                self.eat(",")
            return ("array", es)
        if w == "self":
            self.i += 1
            return ("var", "self")
        self.i += 1
        path = [w]
        if self.at("[") and (w[:1].isupper() and self.is_generic_args() or w in self.fn_names and self.is_generic_args(("(",))):
            self.generics()
        while self.at(".") and self.peek().k == "id" and path[-1][:1].isupper() and not self.peek().nl:
            # Type.method / Enum.Variant / Enum.Variant { .. }
            self.i += 1
            path.append(self.ident())
            if self.at("[") and self.is_generic_args():
                self.generics()
            if not path[-1][:1].isupper():
                break
        if (self.at("{") and path[-1][:1].isupper() and self.looks_like_struct_lit()
                and (not self.no_struct or (self.peek(1).k == "id" and self.peek(2).k == "op" and self.peek(2).v == ":"))):
            return self.struct_lit(".".join(path))
        if len(path) == 1:
            return ("var", w)
        return ("path", path)

    def looks_like_struct_lit(self) -> bool:
        a, b = self.peek(1), self.peek(2)
        if a.k == "op" and a.v == "}":
            return True
        if a.k == "op" and a.v == "..":
            return True
        return a.k == "id" and b.k == "op" and b.v in (":", ",", "}")

    def struct_lit(self, name):
        self.expect("{")
        saved = self.no_struct
        self.no_struct = 0
        fs = []
        base = None
        while not self.eat("}"):
            if self.eat(".."):
                base = self.expr()
                continue
            f = self.ident()
            if self.eat(":"):
                fs.append((f, self.expr()))
            else:
                fs.append((f, ("var", f)))
            self.eat(",")
        self.no_struct = saved
        return ("struct", name, fs, base)

    def cond(self):
        self.no_struct += 1
        if self.at("let"):
            self.i += 1
            pat = self.pattern()
            self.expect("=")
            e = self.expr()
            c = ("let", pat, e)
        else:
            c = self.expr()
        self.no_struct -= 1
        return c

    def if_expr(self):
        self.expect("if")
        cond = self.cond()
        then = self.block()
        els = None
        if self.at("else"):
            self.i += 1
            if self.at("if"):
                els = Block([], self.if_expr())
            else:
                els = self.block()
        return ("if", cond, then, els)

    def closure(self, is_move):
        params = []
        if self.eat("||"):
            pass
        else:
            self.expect("|")
            while not self.eat("|"):
                self.eat("mut")
                p = self.pattern1()
                if self.eat(":"):
                    self.ty()
                params.append(p)
                self.eat(",")
        if self.eat("->"):
            self.ty()
        body = self.expr()
        return ("closure", params, body, is_move)

    def fstring(self, raw: str, line: int):
        parts = []
        buf = []
        i = 0
        while i < len(raw):
            c = raw[i]
            if c == "\\":
                buf.append(raw[i:i + 2])
                i += 2
                continue
            if c == "{" and raw.startswith("{{", i):
                buf.append("{")
                i += 2
                continue
            if c == "}" and raw.startswith("}}", i):
                buf.append("}")
                i += 2
                continue
            if c == "{":
                depth = 1
                j = i + 1
                in_str = False
                while depth:
                    if raw[j] == '"' and raw[j - 1] != "\\":
                        in_str = not in_str
                    elif not in_str and raw[j] == "{":
                        depth += 1
                    elif not in_str and raw[j] == "}":
                        depth -= 1
                    j += 1
                inner = raw[i + 1:j - 1]
                spec = ""
                # a trailing `:spec` at depth 0, outside strings and paths
                m = re.match(r"^(.*?)(?::([^:\"]*))?$", inner, re.S)
                if m and m.group(2) is not None and not inner.rstrip().endswith("::"):
                    head = m.group(1)
                    if (head.count("(") == head.count(")") and head.count("[") == head.count("]")
                            and head.count("{") == head.count("}")):
                        inner, spec = head, m.group(2)
                if buf:
                    parts.append(_unescape("".join(buf)))
                    buf = []
                sub = Parser(inner)
                sub.fn_names = self.fn_names
                e = sub.expr()
                if sub.cur.k != "eof":
                    raise ParseError(f"line {line}: trailing tokens in f-string hole {inner!r}")
                parts.append((e, spec))
                i = j
                continue
            buf.append(c)
            i += 1
        if buf:
            parts.append(_unescape("".join(buf)))
        return parts


def parse(src: str) -> Program:
    return Parser(src).program()
