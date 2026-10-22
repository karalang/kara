"""Fill in a `format!` template whose arguments are known when the test is read.

A test that builds its program with `format!(r#"..."#, a, n = 3)` is only a
template to `extract.py`: the program exists once the holes are filled. When
every argument is a literal, or a `const`, `static` or `let` in scope at the
call with a literal value, the filled program is fixed, and the corpus can
hold it as an ordinary program (M3 item 9, batch 2). Scope is read off the
file's `{ }` blocks, so a `const H` of one test is never taken for another's.

`fill(text, start, content, kind, holder_start)` returns the filled program,
or None when any argument or hole is out of reach: an expression, a format
spec other than `{}` / `{:?}` on a number or bool, a float Rust would print
differently from Python, and so on. None keeps the entry a template.
"""

import re

from rustscan import blank, unescape

RAW = re.compile(r'b?r(#*)"')
INT = re.compile(r"-?(?:0x[0-9a-fA-F_]+|0b[01_]+|0o[0-7_]+|[0-9][0-9_]*)(?:_?(?:[iu](?:8|16|32|64|128|size)))?")
FLOAT = re.compile(r"-?[0-9][0-9_]*\.[0-9][0-9_]*(?:_?f(?:32|64))?")
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
STR = re.compile(r'"(?:[^"\\]|\\.)*"', re.S)
DEF = re.compile(r"\b(const|static|let)\s+(?:mut\s+)?([A-Za-z_]\w*)\s*(?::[^=;]*)?=")


class Unfillable(Exception):
    pass


def _lit_end(text, start, content, kind):
    if kind == "raw":
        m = RAW.match(text, start)
        return m.end() + len(content) + 1 + len(m.group(1))
    return start + 1 + len(content) + 1


def _split_args(text, i):
    """The arguments after the template literal, up to the call's `)`:
    a list of source strings, and the offset after the `)`."""
    args, cur, depth, n = [], [], 0, len(text)
    while i < n:
        c = text[i]
        if c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            cur.append(text[i : j + 1])
            i = j + 1
            continue
        m = RAW.match(text, i) if c in "rb" and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")) else None
        if m:
            end = text.find('"' + m.group(1), m.end())
            if end < 0:
                raise Unfillable("unterminated raw string")
            cur.append(text[i : end + 1 + len(m.group(1))])
            i = end + 1 + len(m.group(1))
            continue
        if c == "'":
            m2 = re.compile(r"'(\\u\{[0-9a-fA-F]+\}|\\.|[^\\'])'").match(text, i)
            if m2:
                cur.append(m2.group(0))
                i = m2.end()
                continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                if c != ")":
                    raise Unfillable("unbalanced")
                a = "".join(cur).strip()
                if a:
                    args.append(a)
                return args, i + 1
            depth -= 1
        elif c == "," and depth == 0:
            args.append("".join(cur).strip())
            cur = []
            i += 1
            continue
        cur.append(c)
        i += 1
    raise Unfillable("no closing paren")


def _value(expr, env):
    """A literal's value as ("int"|"float"|"bool"|"str"|"char", python value)."""
    e = expr.strip()
    while e.startswith("&"):
        e = e[1:].strip()
    if e.startswith("(") and e.endswith(")"):
        return _value(e[1:-1], env)
    if FLOAT.fullmatch(e):
        return "float", float(re.sub(r"_?f(32|64)$", "", e).replace("_", ""))
    if INT.fullmatch(e):
        body = re.sub(r"_?[iu](8|16|32|64|128|size)$", "", e).replace("_", "")
        return "int", int(body, 0)
    if e in ("true", "false"):
        return "bool", e == "true"
    if STR.fullmatch(e):
        return "str", unescape(e[1:-1])
    m = RAW.match(e)
    if m and e.find('"' + m.group(1), m.end()) == len(e) - 1 - len(m.group(1)):
        return "str", e[m.end() : len(e) - 1 - len(m.group(1))]
    if len(e) >= 3 and e[0] == "'" and e[-1] == "'":
        return "char", unescape(e[1:-1])
    for suffix in (".to_string()", ".to_owned()", ".into()"):
        if e.endswith(suffix) and e.startswith('"'):
            return _value(e[: -len(suffix)], env)
    if IDENT.fullmatch(e) and e in env:
        return env[e]
    raise Unfillable(f"argument {e!r}")


def _render(v, spec):
    kind, x = v
    if spec not in ("", "?"):
        raise Unfillable(f"format spec {spec!r}")
    if spec == "?" and kind in ("str", "char"):
        raise Unfillable("Debug of a string")
    if kind == "int":
        return str(x)
    if kind == "bool":
        return "true" if x else "false"
    if kind == "float":
        if x == int(x) and abs(x) < 1e16:
            return str(int(x)) + (".0" if spec == "?" else "")
        r = repr(x)
        if "e" in r or "inf" in r or "nan" in r:
            raise Unfillable("float Rust may print differently")
        return r
    return x


class _File:
    """One test file's code (`blank`), its `{ }` blocks, and every `const`,
    `static` and `let` with the source text of its value."""

    def __init__(self, text):
        self.code = code = blank(text)
        self.blocks, stack = [], []
        for i, c in enumerate(code):
            if c == "{":
                stack.append(i)
            elif c == "}" and stack:
                self.blocks.append((stack.pop(), i))
        self.defs = []
        for m in DEF.finditer(code):
            end, depth = m.end(), 0
            while end < len(code):
                c = code[end]
                if c in "([{":
                    depth += 1
                elif c in ")]}":
                    if depth == 0:
                        break
                    depth -= 1
                elif c == ";" and depth == 0:
                    break
                end += 1
            if end < len(code) and code[end] == ";":
                self.defs.append((m.start(), m.group(1), m.group(2), text[m.end() : end]))
        self.def_block = [self.block(d[0]) for d in self.defs]

    def block(self, p):
        """The innermost block around offset p, or None at the top level."""
        best = None
        for o, c in self.blocks:
            if o < p < c and (best is None or o > best[0]):
                best = (o, c)
        return best

    def env(self, call):
        """The literal values in scope at the call: an item (`const`,
        `static`) anywhere in a block around the call, the innermost one
        winning, and each `let` before it in a block around it, the last one
        winning. A name whose winning definition is not a literal is left out."""
        env, items = {}, {}
        for (p, kind, name, src), blk in zip(self.defs, self.def_block):
            if blk is not None and not (blk[0] < call < blk[1]):
                continue
            if kind == "let":
                continue
            depth = -1 if blk is None else blk[0]
            if name in items and items[name][0] == depth:
                items[name] = (depth, None)  # two at one level: not ours to pick
            elif name not in items or items[name][0] < depth:
                items[name] = (depth, src)
        for name, (_, src) in items.items():
            try:
                env[name] = _value(src, {}) if src is not None else None
            except Unfillable:
                env[name] = None
        for (p, kind, name, src), blk in zip(self.defs, self.def_block):
            if kind != "let" or p > call:
                continue
            if blk is not None and not (blk[0] < call < blk[1]):
                continue
            try:
                env[name] = _value(src, {k: v for k, v in env.items() if v is not None})
            except Unfillable:
                env[name] = None
        return {k: v for k, v in env.items() if v is not None}


_FILES = {}


def _env(text, holder_start, call_start):
    f = _FILES.get(id(text))
    if f is None or f[0] is not text:
        f = _FILES[id(text)] = (text, _File(text))
    env = f[1].env(call_start)
    # A name a closure, a `for` or a pattern binds may shadow the `let`.
    region = f[1].code[holder_start:call_start]
    for name in list(env):
        n = re.escape(name)
        if re.search(r"\|[^|\n]*\b" + n + r"\b[^|\n]*\||\bfor\s+[^{]*\b" + n + r"\b[^{]*\bin\b|\(\s*" + n + r"\s*\)\s*=>"
                     r"|\bSome\(\s*" + n + r"\s*\)|\bfn\s+\w+[^{]*[(,]\s*(?:mut\s+)?" + n + r"\s*:", region):
            del env[name]
    return env


def fill(text, start, content, kind, holder_start):
    try:
        template = content if kind == "raw" else unescape(content)
        end = _lit_end(text, start, content, kind)
        j = end
        while j < len(text) and text[j] in " \t\r\n":
            j += 1
        if j < len(text) and text[j] == ")":
            raw_args = []
        elif j < len(text) and text[j] == ",":
            raw_args, _ = _split_args(text, j + 1)
        else:
            return None
        env = _env(text, holder_start, start)
        pos, named = [], {}
        for a in raw_args:
            m = re.fullmatch(r"([A-Za-z_]\w*)\s*=\s*(.+)", a, re.S)
            if m and not m.group(2).startswith("="):
                named[m.group(1)] = _value(m.group(2), env)
            else:
                pos.append(_value(a, env))
        out, i, nxt, used = [], 0, 0, set()
        while i < len(template):
            c = template[i]
            if template.startswith("{{", i) or template.startswith("}}", i):
                out.append(c)
                i += 2
                continue
            if c == "}":
                return None
            if c != "{":
                out.append(c)
                i += 1
                continue
            k = template.find("}", i)
            if k < 0:
                return None
            inner = template[i + 1 : k]
            arg, _, spec = inner.partition(":")
            arg = arg.strip()
            if arg == "":
                if nxt >= len(pos):
                    return None
                v = pos[nxt]
                used.add(nxt)
                nxt += 1
            elif arg.isdigit():
                if int(arg) >= len(pos):
                    return None
                v = pos[int(arg)]
                used.add(int(arg))
            elif arg in named:
                v = named[arg]
            elif IDENT.fullmatch(arg) and arg in env:
                v = env[arg]
            else:
                return None
            out.append(_render(v, spec))
            i = k + 1
        if len(used) != len(pos):
            return None  # an argument no hole takes: rustc refuses it, so this is not a read we trust
        return "".join(out)
    except (Unfillable, ValueError, IndexError):
        return None
