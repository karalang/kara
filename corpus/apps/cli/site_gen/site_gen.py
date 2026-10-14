# Python mirror of source.kara: same pages, same conversion, same output.
import os

def escape(s):
    out = ""
    for c in s:
        out += {"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;"}.get(c, c)
    return out

def slugify(s):
    out, dash = "", False
    for c in s.lower():
        if c.isalnum():
            out += c
            dash = False
        elif not dash and out:
            out += "-"
            dash = True
    if out.endswith("-"):
        out = out[:-1]
    return out

def inline(text, links):
    cs, out, i, n = text, "", 0, len(text)
    while i < n:
        c = cs[i]
        if c == "`":
            j = i + 1
            while j < n and cs[j] != "`":
                j += 1
            if j < n:
                out += f"<code>{escape(cs[i + 1:j])}</code>"
                i = j + 1
                continue
        if c == "*" and i + 1 < n and cs[i + 1] == "*":
            j = i + 2
            while j + 1 < n and not (cs[j] == "*" and cs[j + 1] == "*"):
                j += 1
            if j + 1 < n:
                out += f"<strong>{inline(cs[i + 2:j], links)}</strong>"
                i = j + 2
                continue
        if c == "*":
            j = i + 1
            while j < n and cs[j] != "*":
                j += 1
            if j < n and j > i + 1:
                out += f"<em>{inline(cs[i + 1:j], links)}</em>"
                i = j + 1
                continue
        if c == "[":
            j = i + 1
            while j < n and cs[j] != "]":
                j += 1
            if j + 1 < n and cs[j + 1] == "(":
                k = j + 2
                while k < n and cs[k] != ")":
                    k += 1
                if k < n:
                    href = cs[j + 2:k]
                    links.append(href)
                    out += f'<a href="{escape(href)}">{inline(cs[i + 1:j], links)}</a>'
                    i = k + 1
                    continue
        out += escape(c)
        i += 1
    return out

def render(slug, md):
    html, headings, links, title = "", [], [], slug
    para, in_list, in_code = [], False, False
    for line in md.splitlines() + [""]:
        if in_code:
            if line.startswith("```"):
                html += "</code></pre>\n"
                in_code = False
            else:
                html += escape(line) + "\n"
            continue
        t = line.strip()
        is_item = t.startswith("- ")
        if (not t or t.startswith("#") or t.startswith("```") or is_item) and para:
            html += f"<p>{inline(' '.join(para), links)}</p>\n"
            para = []
        if in_list and not is_item:
            html += "</ul>\n"
            in_list = False
        if t.startswith("```"):
            lang = t[3:].strip()
            html += "<pre><code>" if not lang else f'<pre><code class="language-{lang}">'
            in_code = True
        elif t.startswith("#"):
            level = len(t) - len(t.lstrip("#"))
            text = t[level:].strip()
            id_ = slugify(text)
            if level == 1:
                title = text
            headings.append((level, text, id_))
            html += f'<h{level} id="{id_}">{inline(text, links)}</h{level}>\n'
        elif is_item:
            if not in_list:
                html += "<ul>\n"
                in_list = True
            html += f"<li>{inline(t[2:], links)}</li>\n"
        elif t:
            para.append(t)
    return {"slug": slug, "title": title, "html": html, "headings": headings, "links": links}

def wrap(site, base, p):
    return (f"<!doctype html>\n<html><head><title>{escape(p['title'])} | {escape(site)}</title>\n"
            f'<base href="{base}"></head>\n<body>\n{p["html"]}</body></html>\n')

PAGES = [
    ("intro", """# Getting started

Kara is a **small** language for *services* & tools.
See [the guide](guide.html) and [the FAQ](faq.html).

## Install

- download the `karac` binary
- put it on your `PATH`
- run `karac --version`

```sh
karac build hello.kara
./hello
```
"""),
    ("guide", """# The guide

## Values & types

A `String` owns its text; a `Str` is a view: `"a" < "b"` is *true*.

## Errors

Use `?` to propagate. Back to [the intro](intro.html), or the [missing page](nowhere.html).
"""),
    ("faq", """# FAQ

## Is it fast?

It compiles to native code through LLVM. See [benchmarks](https://example.com/bench).

## Why <angle brackets>?

They are *escaped*: 1 < 2 && 3 > 2.
"""),
]

def main():
    site = os.environ.get("SITE_TITLE", "Untitled")
    base = os.environ.get("SITE_BASE", "/")
    for slug, md in PAGES:
        open(f"{slug}.md", "w").write(md)
    pages = []
    for slug, _ in PAGES:
        p = render(slug, open(f"{slug}.md").read())
        out = wrap(site, base, p)
        open(f"{slug}.html", "w").write(out)
        print(f"{slug}.html: {len(out.encode())} bytes, {len(p['headings'])} headings, {len(p['links'])} links")
        pages.append(p)
    toc = f"# {site}\n\n"
    for p in pages:
        toc += f"- [{p['title']}]({p['slug']}.html)\n"
        for level, text, id_ in p["headings"]:
            if level == 2:
                toc += f"- [{p['title']}: {text}]({p['slug']}.html#{id_})\n"
    index = render("index", toc)
    open("index.html", "w").write(wrap(site, base, index))
    print(f"index.html: {len(index['links'])} entries")
    known = {f"{p['slug']}.html" for p in pages}
    broken = 0
    for p in pages:
        for l in p["links"]:
            if l.startswith("http://") or l.startswith("https://"):
                continue
            if l.split("#")[0] not in known:
                print(f"broken link in {p['slug']}.md: {l}")
                broken += 1
    print(f"{broken} broken links")
    for line in open("guide.html").read().splitlines():
        if line.startswith("<h") or line.startswith("<p>") or line.startswith("<title>"):
            print(f"  {line}")
    for line in open("faq.html").read().splitlines():
        if line.startswith("<h2") or line.startswith("<p>They"):
            print(f"  {line}")
    show = False
    for line in open("intro.html").read().splitlines():
        if line.startswith("<ul>") or line.startswith("<pre>"):
            show = True
        if show:
            print(f"  {line}")
        if line.startswith("</ul>") or line.endswith("</code></pre>"):
            show = False

main()
