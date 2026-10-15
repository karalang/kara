# mini_gettext

Message catalogs as GNU gettext and Python 3.13's `gettext` module
handle them. Shops, web apps, command-line tools and installers keep
their user-facing text in `.po` files, compile them to binary `.mo`
catalogs, and look each message up at run time with a language
fallback chain and per-language plural rules. The program ports
CPython's `Tools/i18n/msgfmt.py` compiler and `gettext.GNUTranslations`
loader, with `gettext.c2py`'s plural-expression parser, and renders a
small shop's UI through them.

The run has five parts:
- **compile**: ten `.po` files in `data/po`. They use contexts, plural
  entries, fuzzy and untranslated entries, escapes and continuation
  lines, and `pt_BR.po` is Latin-1. Each one is compiled to
  `data/locale/<lang>/LC_MESSAGES/shop.mo`; the files the program
  writes are byte-identical to the ones `msgfmt.py` writes.
- **catalogs**: each compiled catalog loaded back, with its charset,
  entry count, sorted header fields and its plural rule's form for 16
  sample counts.
- **ui**: 14 UI calls (`gettext`, `pgettext`, `ngettext`,
  `npgettext`) from `data/ui.txt`, rendered through the ten fallback
  chains in `data/langs.txt`, such as `pt_BR -> pt` and `xx -> de`.
- **plural forms**: 33 `Plural-Forms` expressions. For each one the
  program prints the Python source `c2py` builds and its value for the
  16 sample counts, or `c2py`'s `ValueError` (bad tokens, unbalanced
  parentheses, too long, too deeply nested). Division and modulo by
  zero print `ZeroDivisionError`, and `&&`, `||` and `?:` evaluate only
  the operands Python would.
- **broken**: seven hand-made `.mo` files in `data/broken`: a
  big-endian catalog, a multi-line header field, a bad plural rule, a
  bad magic number, version numbers 1 and 2, and a corrupt offset
  table. Each prints what `GNUTranslations` loads or the `OSError` or
  `ValueError` it raises.

Not supported: hash tables in `.mo` files (`msgfmt.py` writes none and
the loader ignores them), `gettext.find` and `translation()` lookup on
`LANGUAGE`/`LC_*` and the catalog cache, `install()`, and `.po` files
in charsets other than UTF-8 and Latin-1. The shop UI is the program's
own and is the same in both versions.

**Oracle.** `expected.out` is the output of `oracle.py`, the same
program written with the real `gettext.GNUTranslations`,
`gettext.c2py` and a copy of `msgfmt.py`'s parser and `generate()`.
Run it from this directory after `mk.py` writes `data/`.

## Edits from mini_gettext.orig.kara to mini_gettext.kara

Compiler gaps:
- B-2026-10-14-70: `self.out.msgs` is refused as a chained field
  receiver in `Po.add`. The port moves the loop into a helper,
  `add_msg`, that takes `mut self.out`.

Author errors:
- A `for` element of `lines_of(..)` was pushed into a `Vec` without
  `.clone()`; the element is a borrow.
- The `Option` that `show_catalog` returns was discarded without
  `let _ =` (a warning).

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan`.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
