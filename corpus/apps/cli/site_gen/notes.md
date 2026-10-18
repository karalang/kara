# cli/site_gen

A static site generator: three Markdown pages (a const array of
`(slug, text)` pairs) written to disk and read back, each converted to HTML
(headings with slug ids, paragraphs, lists, fenced code with a language
class, inline code, bold, emphasis, links, HTML escaping, all by a
char-level scanner with recursion for nested markup), wrapped in a page
template, an index page with a table of contents built as Markdown and
rendered the same way, and a check that every internal link names a page
the build wrote. Settings come from the environment (`SITE_TITLE`,
`SITE_BASE`, set in meta.toml's `env`). The mirror `site_gen.py` wrote
`expected.out` with the same environment.

Written 2026-10-14 by the real-world apps thread; karac at c8dad803.

## Edits from source.orig.kara to source.kara

No author errors under the v2 rules. One draft error was fixed before the
first run, since archive had already met its diagnostic: `para.join(\" \")`
inside an f-string interpolation became a `let`.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-14-44: `fs.write` takes `ref String`, so a `Str` page is copied.
- B-2026-10-14-52: `for (slug, md) in PAGES.iter()` over a `const` array runs
  ZERO times on `__mir-llvm-run` and `karac build --mir` (no error, wrong
  output) and is a stack-buffer-overflow under `__mir-llvm-asan`; the
  interpreter is right. `main` iterates a local copy, `let sources = PAGES;`.
- B-2026-10-14-53: `karac check` reports `out` as moved by
  `fs.write(path, out)`, whose content parameter is borrowed; the size is
  read before the write.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.

## Back to the original, 2026-10-18

The rows behind the edits above are fixed, so `site_gen.kara` is now exactly what `karac fix` makes of `site_gen.orig.kara`. It matches expected.out on all four MIR backends (karac at 4ea06e27b9).
