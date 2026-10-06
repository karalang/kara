"""A minimal Rust lexer for finding string literals and item names.

Only what `extract.py` needs: it skips comments, ordinary strings and char
literals, so text INSIDE them is never mistaken for code, and yields

    ("raw", offset, content)        every raw string: r"..", r#".."#, br#".."#, ...
    ("str", offset, content)        every ordinary string (escapes left as written)
    ("decl", offset, kind, name)    every `fn` / `const` / `static` name
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
