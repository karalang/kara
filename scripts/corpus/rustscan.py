"""A minimal Rust lexer for finding string literals and item names.

Only what `extract.py` needs: it skips comments, ordinary strings and char
literals, so text INSIDE them is never mistaken for code, and yields

    ("raw", offset, content)        every raw string: r"..", r#".."#, br#".."#, ...
    ("str", offset, content)        every ordinary string (escapes left as written)
    ("decl", offset, kind, name)    every `fn` / `const` / `static` name

`blank(text)` is the text with every comment, and the inside of every string
and char literal, turned to spaces, newlines kept: the same length, so offsets
carry over, and only code is left to match braces and `;` on. `unescape`
reads an ordinary string's escapes.
"""

import re

DECL = re.compile(r"(fn|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)")
RAW = re.compile(r'b?r(#*)"')
CHAR = re.compile(r"'(\\u\{[0-9a-fA-F]+\}|\\.|[^\\'])'")


def _ident_char(c: str) -> bool:
    return c.isalnum() or c == "_"


def scan(text: str):
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i)
            i = n if j < 0 else j
            continue
        if text.startswith("/*", i):
            depth = 0
            while i < n:
                if text.startswith("/*", i):
                    depth += 1
                    i += 2
                elif text.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
                if depth == 0:
                    break
            continue
        boundary = i == 0 or not _ident_char(text[i - 1])
        if c in "rb" and boundary:
            m = RAW.match(text, i)
            if m:
                hashes = m.group(1)
                start = m.end()
                end = text.find('"' + hashes, start)
                if end < 0:
                    return
                yield ("raw", i, text[start:end])
                i = end + 1 + len(hashes)
                continue
        if c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            yield ("str", i, text[i + 1 : j])
            i = j + 1
            continue
        if c == "'":
            m = CHAR.match(text, i)
            i = m.end() if m else i + 1  # a char literal, or a lifetime
            continue
        if boundary and c in "fcs":
            m = DECL.match(text, i)
            if m:
                yield ("decl", i, m.group(1), m.group(2))
                i = m.end()
                continue
        i += 1


def blank(text: str) -> str:
    out = list(text)

    def wipe(a, b, keep_ends):
        for k in range(a + keep_ends, b - keep_ends):
            if out[k] != "\n":
                out[k] = " "

    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            wipe(i, j, 0)
            i = j
            continue
        if text.startswith("/*", i):
            a, depth = i, 0
            while i < n:
                if text.startswith("/*", i):
                    depth += 1
                    i += 2
                elif text.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
                if depth == 0:
                    break
            wipe(a, i, 0)
            continue
        if c in "rb" and (i == 0 or not _ident_char(text[i - 1])):
            m = RAW.match(text, i)
            if m:
                end = text.find('"' + m.group(1), m.end())
                end = n if end < 0 else end + 1 + len(m.group(1))
                wipe(i, end, 0)
                out[i], out[end - 1] = '"', '"'
                i = end
                continue
        if c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            wipe(i, min(j + 1, n), 1)
            i = j + 1
            continue
        if c == "'":
            m = CHAR.match(text, i)
            if m:
                wipe(i, m.end(), 1)
                i = m.end()
                continue
        i += 1
    return "".join(out)


def unescape(s: str) -> str:
    """Rust ordinary-string escapes, including `\\` + newline continuation."""
    out, i = [], 0
    while i < len(s):
        c = s[i]
        if c != "\\" or i + 1 >= len(s):
            out.append(c)
            i += 1
            continue
        d = s[i + 1]
        i += 2
        if d == "n":
            out.append("\n")
        elif d == "t":
            out.append("\t")
        elif d == "r":
            out.append("\r")
        elif d == "0":
            out.append("\0")
        elif d in "\\\"'":
            out.append(d)
        elif d == "x":
            out.append(chr(int(s[i : i + 2], 16)))
            i += 2
        elif d == "u":
            j = s.index("}", i)
            out.append(chr(int(s[i + 1 : j], 16)))
            i = j + 1
        elif d == "\n":
            while i < len(s) and s[i] in " \t\n\r":
                i += 1
        else:
            out.append("\\" + d)
    return "".join(out)
