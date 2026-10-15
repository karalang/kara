# Mirror of md_render.kara: a CommonMark Markdown-to-HTML renderer and a
# small docs-site build over real files.
import os

PUNCT = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"

def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")

def indent_of(line):
    n = 0
    while n < len(line) and line[n] == " ":
        n += 1
    return n

def is_blank(line):
    return line.strip(" \t") == ""

def strip_cols(line, n):
    # drop up to n leading spaces
    k = 0
    while k < n and k < len(line) and line[k] == " ":
        k += 1
    return line[k:]

def is_digit(c):
    return "0" <= c <= "9"

def is_alnum(c):
    return is_digit(c) or "a" <= c <= "z" or "A" <= c <= "Z"

# ------------------------------------------------------------ block starts

def is_hr(line):
    if indent_of(line) > 3:
        return False
    s = line.strip(" \t")
    if s == "" or s[0] not in "*-_":
        return False
    n = 0
    for c in s:
        if c == s[0]:
            n += 1
        elif c not in " \t":
            return False
    return n >= 3

def atx(line):
    # (level, text) of an ATX heading, else None
    if indent_of(line) > 3:
        return None
    s = line.lstrip(" ")
    n = 0
    while n < len(s) and s[n] == "#":
        n += 1
    if n == 0 or n > 6 or (n < len(s) and s[n] not in " \t"):
        return None
    rest = s[n:].strip(" \t")
    j = len(rest)
    while j > 0 and rest[j - 1] == "#":
        j -= 1
    if j == 0:
        rest = ""
    elif rest[j - 1] in " \t":
        rest = rest[:j].rstrip(" \t")
    return (n, rest)

def fence_open(line):
    ind = indent_of(line)
    if ind > 3:
        return None
    s = line[ind:]
    if s == "" or s[0] not in "`~":
        return None
    n = 0
    while n < len(s) and s[n] == s[0]:
        n += 1
    if n < 3:
        return None
    info = s[n:].strip(" \t")
    if s[0] == "`" and "`" in info:
        return None
    return (s[0], n, ind, info)

def is_fence_close(line, c, n):
    if indent_of(line) > 3:
        return False
    s = line.strip(" \t")
    k = 0
    while k < len(s) and s[k] == c:
        k += 1
    return k >= n and k == len(s)

def quote_content(line):
    ind = indent_of(line)
    if ind > 3 or ind >= len(line) or line[ind] != ">":
        return None
    rest = line[ind + 1:]
    if rest.startswith(" "):
        rest = rest[1:]
    return rest

def list_marker(line):
    # (kind, start, content column, first-line content), else None; kind is
    # the bullet character or the ordered delimiter
    ind = indent_of(line)
    if ind > 3:
        return None
    s = line[ind:]
    start = -1
    if s[:1] in ("-", "+", "*"):
        kind = s[0]
        mlen = 1
    else:
        j = 0
        while j < len(s) and j < 9 and is_digit(s[j]):
            j += 1
        if j == 0 or j >= len(s) or s[j] not in ".)":
            return None
        kind = s[j]
        start = int(s[:j])
        mlen = j + 1
    after = s[mlen:]
    if after != "" and after[0] != " ":
        return None
    sp = indent_of(after)
    if sp == len(after):
        return (kind, start, ind + mlen + 1, "")
    if sp >= 5:
        return (kind, start, ind + mlen + 1, after[1:])
    return (kind, start, ind + mlen + sp, after[sp:])

def setext(line):
    if indent_of(line) > 3:
        return 0
    s = line.strip(" \t")
    if s != "" and s.strip("=") == "":
        return 1
    if s != "" and s.strip("-") == "":
        return 2
    return 0

def interrupts(line):
    # whether the line starts a block that can interrupt a paragraph
    if atx(line) or fence_open(line) or is_hr(line) or quote_content(line) is not None:
        return True
    m = list_marker(line)
    if m is None or m[3] == "":
        return False
    return m[1] < 0 or m[1] == 1

def norm_label(s):
    return " ".join(s.lower().split())

# ------------------------------------------------------------ blocks

# A block is a tuple: ("p", text) ("h", level, text) ("hr",)
# ("code", lang, text) ("quote", blocks) ("list", ordered, start, tight, items)

class Blocks:
    def __init__(self):
        self.refs = {}

    def ref_def(self, line):
        # a one-line link reference definition, recorded; False if it is not one
        s = line.strip(" ")
        if not s.startswith("["):
            return False
        k = s.find("]:")
        if k < 2 or "[" in s[1:k] or "]" in s[1:k]:
            return False
        label = s[1:k]
        rest = s[k + 2:].strip(" ")
        if rest == "":
            return False
        if rest.startswith("<"):
            e = rest.find(">")
            if e < 0:
                return False
            dest = rest[1:e]
            rest = rest[e + 1:]
        else:
            e = 0
            while e < len(rest) and rest[e] != " ":
                e += 1
            dest = rest[:e]
            rest = rest[e:]
        title = ""
        t = rest.strip(" ")
        if t != "":
            if len(t) < 2 or t[0] not in "\"'(" or t[-1] != {"\"": "\"", "'": "'", "(": ")"}[t[0]] or rest[0] != " ":
                return False
            title = t[1:-1]
        key = norm_label(label)
        if key == "":
            return False
        if key not in self.refs:
            self.refs[key] = (unescape(dest), unescape(title))
        return True

    def parse(self, lines):
        # (blocks, gap): gap is whether a blank line separates two of them
        blocks = []
        gap = False
        blank = False
        i = 0
        n = len(lines)
        while i < n:
            line = lines[i]
            if is_blank(line):
                blank = True
                i += 1
                continue
            if blocks and blank:
                gap = True
            blank = False
            if indent_of(line) >= 4:
                buf = []
                while i < n and (is_blank(lines[i]) or indent_of(lines[i]) >= 4):
                    buf.append(strip_cols(lines[i], 4))
                    i += 1
                while buf and is_blank(buf[-1]):
                    buf.pop()
                    blank = True
                blocks.append(("code", "", "".join(x + "\n" for x in buf)))
                continue
            f = fence_open(line)
            if f:
                c, flen, find, info = f
                i += 1
                buf = []
                while i < n:
                    if is_fence_close(lines[i], c, flen):
                        i += 1
                        break
                    buf.append(strip_cols(lines[i], find))
                    i += 1
                lang = info.split(" ")[0] if info else ""
                blocks.append(("code", unescape(lang), "".join(x + "\n" for x in buf)))
                continue
            h = atx(line)
            if h:
                blocks.append(("h", h[0], h[1]))
                i += 1
                continue
            if is_hr(line):
                blocks.append(("hr",))
                i += 1
                continue
            if quote_content(line) is not None:
                buf = []
                while i < n and quote_content(lines[i]) is not None:
                    buf.append(quote_content(lines[i]))
                    i += 1
                sub, _ = self.parse(buf)
                blocks.append(("quote", sub))
                continue
            m = list_marker(line)
            if m:
                kind = m[0]
                start = m[1]
                items = []
                loose = False
                trailing = 0
                while i < n:
                    m = list_marker(lines[i])
                    if m is None or m[0] != kind or is_hr(lines[i]):
                        break
                    if trailing:
                        loose = True
                    w = m[2]
                    buf = [m[3]]
                    i += 1
                    while i < n and (is_blank(lines[i]) or indent_of(lines[i]) >= w):
                        buf.append("" if is_blank(lines[i]) else lines[i][w:])
                        i += 1
                    trailing = 0
                    while len(buf) > 1 and buf[-1] == "":
                        buf.pop()
                        trailing += 1
                    sub, inner_gap = self.parse(buf)
                    if inner_gap:
                        loose = True
                    items.append(sub)
                blocks.append(("list", kind in ".)", start, not loose, items))
                if trailing:
                    blank = True
                continue
            buf = [line]
            i += 1
            level = 0
            while i < n and not is_blank(lines[i]):
                level = setext(lines[i])
                if level:
                    i += 1
                    break
                if interrupts(lines[i]):
                    break
                buf.append(lines[i])
                i += 1
            while buf and self.ref_def(buf[0]):
                buf.pop(0)
            if not buf:
                continue
            text = "\n".join(x.lstrip(" \t") for x in buf).rstrip(" \t")
            if level:
                blocks.append(("h", level, text))
            else:
                blocks.append(("p", text))
        return blocks, gap

def unescape(s):
    out = ""
    i = 0
    while i < len(s):
        if s[i] == "\\" and i + 1 < len(s) and s[i + 1] in PUNCT:
            out += s[i + 1]
            i += 2
        else:
            out += s[i]
            i += 1
    return out

# ------------------------------------------------------------ links

SAFE = ";/?:@&=+$,-_.!~*'()#"
HEXD = "0123456789ABCDEFabcdef"

def encode_url(s):
    # mdurl.encode: keep what a URL may hold, %XX the rest as UTF-8
    out = ""
    b = s.encode("utf-8")
    i = 0
    while i < len(b):
        c = chr(b[i])
        if c == "%" and i + 2 < len(b) and chr(b[i + 1]) in HEXD and chr(b[i + 2]) in HEXD:
            out += c + chr(b[i + 1]) + chr(b[i + 2])
            i += 3
            continue
        if b[i] < 128 and (is_alnum(c) or c in SAFE):
            out += c
        else:
            out += "%" + format(b[i], "02X")
        i += 1
    return out

# ------------------------------------------------------------ inlines

# A token is [kind, text, opens, closes]; kind is "text", "html", "code",
# "delim" or "bracket". A delimiter is a dict over a "delim" token.

def is_ws(c):
    return c == "" or c in " \t\n"

def is_punct(c):
    return c != "" and c in PUNCT

class Inline:
    def __init__(self, src, refs):
        self.s = src
        self.p = 0
        self.refs = refs
        self.toks = []
        self.delims = []
        self.brackets = []

    def at(self, i):
        return self.s[i] if 0 <= i < len(self.s) else ""

    def text(self, t):
        if self.toks and self.toks[-1][0] == "text":
            self.toks[-1][1] += t
        else:
            self.toks.append(["text", t, [], []])

    def html(self, t):
        self.toks.append(["html", t, [], []])

    def run(self):
        while self.p < len(self.s):
            c = self.s[self.p]
            if c == "\\":
                nx = self.at(self.p + 1)
                if nx == "\n":
                    self.p += 2
                    self.html("<br />\n")
                    self.skip_spaces()
                elif is_punct(nx):
                    self.text(nx)
                    self.p += 2
                else:
                    self.text("\\")
                    self.p += 1
            elif c == "`":
                self.code_span()
            elif c == "*" or c == "_":
                self.delim_run()
            elif c == "[":
                self.brackets.append({"tok": len(self.toks), "image": False, "active": True, "dbot": len(self.delims), "src": self.p + 1})
                self.toks.append(["bracket", "[", [], []])
                self.p += 1
            elif c == "!" and self.at(self.p + 1) == "[":
                self.brackets.append({"tok": len(self.toks), "image": True, "active": True, "dbot": len(self.delims), "src": self.p + 2})
                self.toks.append(["bracket", "![", [], []])
                self.p += 2
            elif c == "]":
                self.close_bracket()
            elif c == "<" and self.autolink():
                pass
            elif c == "\n":
                spaces = 0
                if self.toks and self.toks[-1][0] == "text":
                    t = self.toks[-1][1]
                    spaces = len(t) - len(t.rstrip(" "))
                    self.toks[-1][1] = t.rstrip(" ")
                self.html("<br />\n" if spaces >= 2 else "\n")
                self.p += 1
                self.skip_spaces()
            else:
                self.text(c)
                self.p += 1
        self.process_emphasis(0)
        return self.render(0, False)

    def skip_spaces(self):
        while self.at(self.p) == " ":
            self.p += 1

    def code_span(self):
        st = self.p
        n = 0
        while self.at(st + n) == "`":
            n += 1
        j = st + n
        while True:
            k = self.s.find("`", j)
            if k < 0:
                self.text("`" * n)
                self.p = st + n
                return
            m = 0
            while self.at(k + m) == "`":
                m += 1
            if m == n:
                body = self.s[st + n:k].replace("\n", " ")
                if len(body) >= 2 and body[0] == " " and body[-1] == " " and body.strip(" ") != "":
                    body = body[1:-1]
                self.toks.append(["code", body, [], []])
                self.p = k + m
                return
            j = k + m

    def delim_run(self):
        c = self.s[self.p]
        st = self.p
        while self.at(self.p) == c:
            self.p += 1
        n = self.p - st
        before = self.at(st - 1)
        after = self.at(self.p)
        left = not is_ws(after) and (not is_punct(after) or is_ws(before) or is_punct(before))
        right = not is_ws(before) and (not is_punct(before) or is_ws(after) or is_punct(after))
        if c == "*":
            can_open, can_close = left, right
        else:
            can_open = left and (not right or is_punct(before))
            can_close = right and (not left or is_punct(after))
        self.delims.append({"tok": len(self.toks), "ch": c, "n": n, "orig": n, "open": can_open, "close": can_close, "live": True})
        self.toks.append(["delim", c * n, [], []])

    def process_emphasis(self, bottom):
        d = self.delims
        obot = {}
        cur = bottom
        while cur < len(d):
            c = d[cur]
            if not c["live"] or not c["close"]:
                cur += 1
                continue
            key = (c["ch"], c["open"], c["orig"] % 3)
            lo = obot.get(key, bottom - 1)
            j = cur - 1
            found = -1
            while j > lo:
                o = d[j]
                if o["live"] and o["open"] and o["ch"] == c["ch"]:
                    odd = (o["close"] or c["open"]) and (o["orig"] + c["orig"]) % 3 == 0 and not (o["orig"] % 3 == 0 and c["orig"] % 3 == 0)
                    if not odd:
                        found = j
                        break
                j -= 1
            if found < 0:
                obot[key] = cur - 1
                if not c["open"]:
                    c["live"] = False
                cur += 1
                continue
            o = d[found]
            use = 2 if o["n"] >= 2 and c["n"] >= 2 else 1
            tag = "strong" if use == 2 else "em"
            o["n"] -= use
            c["n"] -= use
            self.toks[o["tok"]][1] = o["ch"] * o["n"]
            self.toks[c["tok"]][1] = c["ch"] * c["n"]
            self.toks[o["tok"]][2].insert(0, f"<{tag}>")
            self.toks[c["tok"]][3].append(f"</{tag}>")
            for k in range(found + 1, cur):
                d[k]["live"] = False
            if o["n"] == 0:
                o["live"] = False
            if c["n"] == 0:
                c["live"] = False
                cur += 1
        del d[bottom:]

    def close_bracket(self):
        if not self.brackets:
            self.text("]")
            self.p += 1
            return
        b = self.brackets[-1]
        if not b["active"]:
            self.brackets.pop()
            self.text("]")
            self.p += 1
            return
        res = self.inline_link(self.p + 1)
        if res is None:
            q = self.p + 1
            label = self.s[b["src"]:self.p]
            nextp = q
            if self.at(q) == "[":
                k = self.s.find("]", q + 1)
                if k >= 0 and "[" not in self.s[q + 1:k]:
                    if self.s[q + 1:k].strip(" \n") != "":
                        label = self.s[q + 1:k]
                    nextp = k + 1
            key = norm_label(label)
            if key in self.refs:
                res = (self.refs[key][0], self.refs[key][1], nextp)
        if res is None:
            self.brackets.pop()
            self.text("]")
            self.p += 1
            return
        href, title, nextp = res
        self.process_emphasis(b["dbot"])
        attrs = f' title="{esc(title)}"' if title else ""
        if b["image"]:
            alt = self.render(b["tok"] + 1, True)
            del self.toks[b["tok"]:]
            self.html(f'<img src="{esc(encode_url(href))}" alt="{esc(alt)}"{attrs} />')
        else:
            self.toks[b["tok"]] = ["html", f'<a href="{esc(encode_url(href))}"{attrs}>', [], []]
            self.html("</a>")
            for x in self.brackets[:-1]:
                if not x["image"]:
                    x["active"] = False
        self.brackets.pop()
        self.p = nextp

    def inline_link(self, q):
        # (dest, title, end) of `(dest "title")` at q, else None
        if self.at(q) != "(":
            return None
        q += 1
        while is_ws(self.at(q)) and self.at(q) != "":
            q += 1
        dest = ""
        if self.at(q) == "<":
            e = q + 1
            while self.at(e) not in (">", "", "\n", "<"):
                e += 2 if self.at(e) == "\\" else 1
            if self.at(e) != ">":
                return None
            dest = self.s[q + 1:e]
            q = e + 1
        else:
            st = q
            depth = 0
            while True:
                c = self.at(q)
                if c == "" or c in " \t\n":
                    break
                if c == "\\" and is_punct(self.at(q + 1)):
                    q += 2
                    continue
                if c == "(":
                    depth += 1
                elif c == ")":
                    if depth == 0:
                        break
                    depth -= 1
                q += 1
            dest = self.s[st:q]
        title = ""
        sp = q
        while is_ws(self.at(q)) and self.at(q) != "":
            q += 1
        c = self.at(q)
        if q > sp and c in ("\"", "'", "("):
            close = ")" if c == "(" else c
            e = q + 1
            while self.at(e) != close:
                if self.at(e) == "":
                    return None
                e += 2 if self.at(e) == "\\" and self.at(e + 1) != "" else 1
            title = self.s[q + 1:e]
            q = e + 1
            while is_ws(self.at(q)) and self.at(q) != "":
                q += 1
        if self.at(q) != ")":
            return None
        return (unescape(dest), unescape(title), q + 1)

    def autolink(self):
        q = self.p + 1
        st = q
        while is_alnum(self.at(q)) or self.at(q) in ("+", ".", "-"):
            q += 1
        scheme = q - st
        if scheme >= 2 and scheme <= 32 and is_alnum(self.at(st)) and not is_digit(self.at(st)) and self.at(q) == ":":
            while self.at(q) not in ("", ">", "<", " ", "\n", "\t"):
                q += 1
            if self.at(q) != ">":
                return False
            url = self.s[st:q]
            self.html(f'<a href="{esc(encode_url(url))}">{esc(url)}</a>')
            self.p = q + 1
            return True
        return False

    def render(self, frm, plain):
        out = ""
        for t in self.toks[frm:]:
            kind, x = t[0], t[1]
            if plain:
                if kind != "html":
                    out += x
                continue
            if kind == "html":
                out += x
            elif kind == "code":
                out += "<code>" + esc(x) + "</code>"
            else:
                out += "".join(t[3]) + esc(x) + "".join(t[2])
        return out

def inline(text, refs):
    return Inline(text, refs).run()

def plain_text(text, refs):
    il = Inline(text, refs)
    il.run()
    return il.render(0, True)

# ------------------------------------------------------------ rendering

class Html:
    def __init__(self, refs):
        self.refs = refs
        self.out = []
        self.headings = []   # (level, plain text)

    def blocks(self, blocks, tight):
        for i, b in enumerate(blocks):
            kind = b[0]
            if kind == "p":
                if tight:
                    self.out.append(inline(b[1], self.refs))
                    if i + 1 < len(blocks):
                        self.out.append("\n")
                else:
                    self.out.append(f"<p>{inline(b[1], self.refs)}</p>\n")
            elif kind == "h":
                self.headings.append((b[1], plain_text(b[2], self.refs)))
                self.out.append(f"<h{b[1]}>{inline(b[2], self.refs)}</h{b[1]}>\n")
            elif kind == "hr":
                self.out.append("<hr />\n")
            elif kind == "code":
                cls = f' class="language-{esc(b[1])}"' if b[1] else ""
                self.out.append(f"<pre><code{cls}>{esc(b[2])}</code></pre>\n")
            elif kind == "quote":
                self.out.append("<blockquote>\n")
                self.blocks(b[1], False)
                self.out.append("</blockquote>\n")
            else:
                _, ordered, start, tight_list, items = b
                if ordered:
                    self.out.append("<ol>\n" if start == 1 else f'<ol start="{start}">\n')
                else:
                    self.out.append("<ul>\n")
                for item in items:
                    self.out.append("<li>")
                    if item and (not tight_list or item[0][0] != "p"):
                        self.out.append("\n")
                    self.blocks(item, tight_list)
                    self.out.append("</li>\n")
                self.out.append("</ol>\n" if ordered else "</ul>\n")

def render_md(src):
    lines = src.replace("\r\n", "\n").split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    bp = Blocks()
    blocks, _ = bp.parse(lines)
    h = Html(bp.refs)
    h.blocks(blocks, False)
    return "".join(h.out), h.headings, len(blocks)

# ------------------------------------------------------------ the site

def slug(text, seen):
    s = ""
    for c in text.lower():
        if is_alnum(c) or c == "-" or c == "_":
            s += c
        elif c == " ":
            s += "-"
    base = s
    n = seen.get(base, 0)
    seen[base] = n + 1
    return base if n == 0 else f"{base}-{n}"

def site_page(body, headings, title):
    seen = {}
    slugs = [slug(t, seen) for _, t in headings]
    # give each heading its id, in order
    out = ""
    i = 0
    k = 0
    while True:
        j = body.find("<h", i)
        if j < 0 or k >= len(slugs):
            out += body[i:]
            break
        if j + 3 < len(body) and is_digit(body[j + 2]) and body[j + 3] == ">":
            out += body[i:j + 3] + f' id="{slugs[k]}">'
            k += 1
            i = j + 4
        else:
            out += body[i:j + 2]
            i = j + 2
    # links to sibling .md files point at their pages
    out = out.replace('.md"', '.html"').replace(".md#", ".html#")
    nav = "".join(f'<li class="h{lv}"><a href="#{s}">{esc(t)}</a></li>\n' for (lv, t), s in zip(headings, slugs))
    page = ("<!doctype html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n"
            f"<title>{esc(title)}</title>\n</head>\n<body>\n<nav>\n<ul>\n{nav}</ul>\n</nav>\n"
            f"<main>\n{out}</main>\n</body>\n</html>\n")
    return page, slugs

# ------------------------------------------------------------ the documents

DOCS = {}

DOCS["README.md"] = r'''# Kara Tools

[![build](https://ci.example.com/badge.svg)](https://ci.example.com/kara)
A set of *small*, **fast** command-line tools for working with logs,
config files and archives. See the [guide](guide.md) and the
[changelog](CHANGELOG.md#unreleased) for details.

## Install

Download a release, or build from source:

```sh
git clone https://example.com/kara-tools.git
cd kara-tools && make install PREFIX=~/.local
```

You need:

- a C compiler (`cc` or `clang`)
- GNU make 4.0 or newer
- about 50 MB of disk

## Usage

    kt grep -i error app.log
    kt toml check Cargo.toml

Every tool reads `stdin` when no file is given, and exits with status
`1` when it finds nothing. Flags follow the usual rules:

1. Short flags combine: `-iv` is `-i -v`.
2. A `--` ends the flags.
3. Long flags take `=`: `--color=never`.

## Contributing

Bug reports are welcome at <https://example.com/kara-tools/issues>.
Please read [the contributing notes][contrib] first, and run the tests
with `make check`.

Licensed under the MIT license. Copyright (c) 2026 the Kara authors.

[contrib]: https://example.com/kara-tools/CONTRIBUTING "How to contribute"
'''

DOCS["CHANGELOG.md"] = r'''Changelog
=========

All notable changes to this project are documented here. The format is
based on *Keep a Changelog*, and versions follow semantic versioning.

Unreleased
----------

### Added

- `kt toml check` validates TOML 1.0 files and reports the line of the
  first error.
- `kt grep` learned `--count` and `--files-with-matches`.

### Fixed

- Archives larger than 4 GB are no longer truncated (#212).
- `kt rotate` keeps the file mode of the log it rotates.

## 1.4.0 - 2026-09-30

### Changed

* The default compression level is now 6, not 9. Level 9 was **twice as
  slow** for a gain of under 1%.
* `--quiet` is now `-q`; the old spelling still works.

### Removed

* The `kt legacy` command, deprecated since 1.1.

***

Older releases are listed in [HISTORY](HISTORY.md).
'''

DOCS["guide.md"] = r'''# User guide

> **Note:** this guide covers version 1.4. Commands marked *(new)* need
> the unreleased build.

## Searching logs

The `kt grep` command takes a pattern and a list of files:

```console
$ kt grep -c "timeout" /var/log/app/*.log
app-1.log:3
app-2.log:0
```

A pattern is matched against each line. These rules apply:

- Matching is case-sensitive unless you pass `-i`.

- A pattern that starts with `-` must follow `--`.

  For example, `kt grep -- -v notes.txt` searches for `-v`.

- Use ``-e `pat` `` to pass a pattern holding backticks.

### Exit codes

3. Found at least one match: the status is `0`.
4. Found nothing: the status is `1`.
5. An error: the status is `2`.

## Rotating files

To rotate `app.log` when it reaches 10 MB, keeping five old copies:

    kt rotate --size 10M --keep 5 app.log

Rotation renames `app.log` to `app.log.1`, shifts the older copies up
by one, and deletes the copy past the limit.\
The rename is atomic, so a reader never sees a missing file.

## Emphasis, briefly

Text can be *emphasised*, _also like this_, or **strong**. Mixed forms
nest: ***both at once***, *emphasis with **strong** inside*, and
**strong with *emphasis* inside**. Underscores inside words do not
count, so `snake_case_name` and snake_case_name stay as they are, and
2 * 3 * 4 is arithmetic. A literal \*star\* needs a backslash.

Two spaces at the end of a line  
make a hard break.

## Images and links

![Architecture diagram](img/arch.png "How the tools fit together")

Links can be [inline](https://example.com/docs "Docs home"),
[by reference][ref], [collapsed][] or just [shortcut]. A path with
spaces needs angle brackets: [notes](<docs/release notes.md>).

[ref]: https://example.com/ref
[collapsed]: https://example.com/collapsed
[shortcut]: https://example.com/shortcut 'Shortcut title'

* * *

Questions? Open an issue.
'''

CASES = [
    "*foo bar*",
    "a * foo bar*",
    "_foo bar_",
    "foo_bar_",
    "*foo**bar**baz*",
    "*foo**bar*",
    "**foo*bar*baz**",
    "foo***bar***baz",
    "*(*foo*)*",
    "__foo, __bar__, baz__",
    "**foo \"*bar*\" foo**",
    "*foo [bar](/url)*",
    "[link *foo **bar** `#`*](/uri)",
    "[a [b] c](/u)",
    "[a [b](/inner) c](/outer)",
    "![a *b* c](/img.png)",
    "`` foo ` bar ``",
    "` `` `",
    "foo\\\nbar",
    "a \\* b \\_ c \\[d\\]",
    "<http://foo.bar.baz/test?q=hello&id=22&boolean>",
    "[x](/url \"title \\\"q\\\"\")",
    "[x](</my uri>)",
    "[x](foo(and(bar)))",
    "[x](/café)",
    "- one\n- two\n\n- three",
    "1. a\n\n   b\n2. c",
    "- a\n  - b\n    - c\n- d",
    "> a\n> - b\n> - c",
    "# Title #\n## Second ##########\n### Third ###x",
    "Heading\n===\nSub\n---\ntext",
    "***\n---\n___",
    "~~~ python extra\nprint(1)\n~~~",
    "````\n```\nnested\n```\n````",
    "*a `*` b*",
    "_foo_bar_baz_",
    "**a**b**c**",
    "[Foo Bar]: /x\n\n[foo   bar] and [FOO BAR][]",
    "1. a\n1. b\n10. c",
    "- a\n* b\n+ c",
    "text\n- item\n2. not a start",
    "[no link] and [also](missing",
]

def main():
    os.makedirs("docs", exist_ok=True)
    os.makedirs("site", exist_ok=True)
    for name, text in DOCS.items():
        with open(f"docs/{name}", "w") as f:
            f.write(text)
    for name in sorted(os.listdir("docs")):
        src = open(f"docs/{name}").read()
        body, headings, nblocks = render_md(src)
        title = headings[0][1] if headings else name
        page, slugs = site_page(body, headings, title)
        stem = name[:-3]
        with open(f"site/{stem}.html", "w") as f:
            f.write(page)
        links = body.count("<a ")
        print(f"== {name}: {len(src.encode())} bytes, {nblocks} blocks, {len(headings)} headings, {links} links")
        print(f"-- site/{stem}.html: {len(page.encode())} bytes")
        for (lv, t), s in zip(headings, slugs):
            print(f"  {'  ' * (lv - 1)}{t}  #{s}")
        print("-- body")
        print(body, end="")
    print("== cases")
    for i, c in enumerate(CASES):
        body, _, _ = render_md(c)
        print(f"-- {i:02}")
        print(body, end="")

if __name__ == "__main__":
    main()
