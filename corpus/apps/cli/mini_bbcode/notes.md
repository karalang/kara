# mini_bbcode

A port of [bbcode](https://pypi.org/project/bbcode/) 1.1.0 (BSD-2-Clause, Dan
Watson): the forum-markup renderer that turns `[b]…[/b]`-style BBCode into HTML.
BBCode is the markup of phpBB, vBulletin and countless message boards; the
library is a single-file, dependency-free parser with a small stack-based
tokenizer and a table of per-tag render functions.

## What it does

`Parser.format(text)` tokenizes the input into tag-start / tag-end / newline /
data tokens, matches each opening tag to its closing token (honouring nesting,
newline-closed tags like `[*]`, and tags that do not render their contents like
`[code]`), then renders the tree through the installed formatters:
`b` `i` `u` `s` `sub` `sup` `hr` `list`/`*` `quote` `code` `center` `color`
`url`. Along the way it escapes HTML (`<`, `>`, `&`, `"`, `'`), applies cosmetic
replacements (`---` → em-dash, `(c)` → `&copy;`, …) and converts newlines to
`<br />`.

`mk.py` writes `data/jobs.tsv`: a sequence of named BBCode inputs in a block
format that preserves significant blank lines (list and code bodies span
lines). `oracle.py` renders each job through the library; `mini_bbcode.kara`
renders it through the port. Both print, per job,

```
=== bold ===
<strong>hello</strong>
```

— the job name, then the rendered HTML on one line with backslash, newline and
tab backslash-escaped so a `[code]` block that keeps its real newlines stays on
a single, unambiguous line.

The 34 jobs exercise nesting, HTML escaping, cosmetic replacement, newline
transformation, the standalone `[hr]`, ordered/unordered `[list]`, the
newline-and-same-tag-closed `[*]`, whitespace-stripping `[quote]`, the
non-embedding `[code]`, `[color=…]` (named, 3/6-digit hex, free-standing, and
rejected), `[url]` (absolute, option form, bare-domain, blocked
`javascript:`, relative path, escaped option), unrecognized and unclosed tags,
stray brackets in plain text, and option parsing with quoted values.

## How the port works

The tokenizer and the recursive renderer are transcribed from `bbcode.py`:

- **`tokenize`** scans for `[`, finds each tag's extent with the library's
  quote-aware `_tag_extent`, validates it with `_parse_tag`, and splits the
  surrounding text into data/newline tokens. Only recognized tags become
  tag tokens; anything else is literal data, so `array[0]` and `[blink]` pass
  through untouched.
- **Option parsing** (`_parse_opts`) is the library's exact character state
  machine — quoted values, escaped quotes, free-standing options, and the
  tag-name-as-option (`[color=red]`, `[url=… ]`) — backed by a
  case-insensitive, insertion-ordered option store.
- **`bb_format`** walks the token range, finds each tag's closing token
  (`find_closing`, with the embed/newline/same-tag rules intact), renders the
  contents (recursively for embedding tags, or as concatenated raw text for
  `[code]`), strips and swallows trailing newlines where the tag asks, and runs
  the render function. Newlines are carried as a `\r` placeholder and swapped
  for `<br />` once at the end, exactly as the library does.

Tokens are matched left-to-right and tags are looked up by name — never an
iteration over a hash map — so the output is deterministic and `run == build`
holds. Everything is byte/codepoint level, so the four backends agree by
construction.

## Scope

- **`replace_links=False` (documented boundary).** The library's bare-text URL
  autolinker — the feature that scans plain text for `http://…` and `www.…` and
  wraps them in `<a>` — is the one part of the parser that needs a real regex
  engine (a catastrophic-backtracking-tuned URL pattern). It is turned off for
  the whole parser, which is a supported `Parser(replace_links=False)` mode, and
  the oracle uses the same setting. Links written explicitly with `[url]` are
  fully supported.
- The two small regexes the core still needs are hand-coded and proven equal to
  the library over the job set: the `[color]` validator (a leading run of
  letters, or `#` + 3–6 hex digits, else `inherit`) and the `[url]` scheme
  sanitiser + bare-domain test (which prepends `http://` to something that looks
  like a domain and blocks `javascript:`/`data:`/`vbscript:`).
- Input is ASCII, which is what the job set and the HTML entities need.
- The `linker`, `drop_unrecognized`, custom `tag_opener`/`tag_closer`,
  `max_tag_depth` and user-added formatters of the library are out of scope;
  this is the default parser with its default formatters.

The port was checked against the library on every backend over the job stream,
and separately a restricted-op reimplementation was compared job-for-job against
`bbcode.Parser(replace_links=False)` during development.

The vendored library package imports only the standard library; `vendor/` is
otherwise left untouched.

## Running

```bash
karac run mini_bbcode.kara                # interpret
karac build --mir mini_bbcode.kara        # native binary
python3 -s -P -B mk.py                         # regenerate data/jobs.tsv
python3 -I oracle.py                            # regenerate expected.out
python3 -s -P -B gen_kara.py                    # regenerate the embedded jobs
```
