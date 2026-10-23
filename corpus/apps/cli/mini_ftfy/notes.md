# mini_ftfy

A port of [ftfy](https://pypi.org/project/ftfy/) 6.3.1 ("fixes text for
you", Apache-2.0, Robyn Speer): the library that repairs mojibake and the
other ways text gets broken between systems.

```
> fix_and_explain('voilÃ le travail')
  -> ExplainedText(text='voilà le travail', explanation=[('encode', 'latin-1'), ('transcode', 'restore_byte_a0'), ('decode', 'utf-8')])
> fix_text('Ð¿Ñ€Ð¸Ð²ÐµÑ‚')
  -> 'привет'
```

## What it does

`mk.py` writes 13 jobs of 822 calls to `data/jobs.tsv`: the cleanup pass a
support desk runs over what its inboxes, web forms and old exports collect
(customer messages, reviews, names and addresses in a dozen languages).
`oracle.py` replays them through the vendored library, and `mini_ftfy.kara`
replays them through the port. Each call prints with its arguments as
`repr()` shows them, then its result or `error <Class>: <message>`.

- **Support inbox.** 49 texts, the library's own examples among them, through
  `fix_text()` and `fix_and_explain()`: mojibake of Latin, Cyrillic, Greek,
  Japanese and emoji, HTML entities (including all-caps ones), terminal
  escapes, a byte order mark, every kind of line break, ligatures,
  halfwidth and fullwidth forms, curly quotes, control characters and
  combining marks.
- **Wrong codec.** 25 messages encoded as UTF-8 and decoded with each of
  the ten codecs ftfy knows (Latin-1, Windows-1250 to 1254 and 1257,
  ISO-8859-2, MacRoman and CP437), as the mojibake is made in the wild.
- **Twice wrong, lost bytes, mixed lines.** The same done twice; with the
  bytes a codec has no character for replaced by U+FFFD or "?"; with
  U+00A0 turned into a space; and good, broken and differently broken
  lines in one text, which `fix_text()` fixes line by line (also split by
  `max_decode_length`).
- **Options.** Every `TextFixerConfig` field switched off, or set to each
  other value, on four samples; an unknown field and an unknown
  normalization form; bytes instead of text.
- **Normalization.** NFC, NFD, NFKC and NFKD over combining sequences,
  Hangul, singletons, excluded compositions and compatibility characters.
- **Each fixer alone,** including the two byte-level ones and
  `decode_escapes()` with its errors; **badness** counts on clean and
  broken text; **plans** found by `fix_encoding_and_explain()` and
  replayed with `apply_plan()`, with its errors; **`guess_bytes()`** on
  UTF-8, UTF-16, CESU-8, MacRoman and Windows-1252 bytes.
- **Tables.** `chardata.WIDTH_MAP` in full, and the all-caps names
  `chardata.HTML_ENTITIES` adds, so the port's builders are checked entry
  by entry.

## How the port works

- **The regular expressions are the library's own.** `gen_kara.py` copies
  the source of each of the 13 patterns off the vendored library into
  `regex_text()`, and the port parses and compiles them itself for a small
  backtracking matcher that tries alternatives in order and repeats
  greedily, as Python's `re` does. It covers what these patterns use:
  classes, `\s \w \d`, lookahead and lookbehind, groups, `^ $ .`, and the
  usual quantifiers, over code points for a `str` pattern and over bytes
  for a `bytes` one.
- **The codecs are tables.** `gen_kara.py` writes Python's decoding table
  for each single-byte codec, and the port builds the "sloppy" ones from
  them as `bad_codecs/sloppy.py` does. UTF-8, `utf-8-variants` (CESU-8 and
  Java's null) and UTF-16 are written out, with CPython's error messages.
- **Unicode normalization is written out** from `unicodedata`'s tables
  (decompositions, combining classes, composition exclusions, with Hangul
  computed), and so is `str.isprintable()` for `repr()`.
- **`HTML_ENTITIES` and `WIDTH_MAP` are built at start-up** as `chardata`
  builds them, from `html.entities.html5` and from NFKC.
- **The library's quirks are kept.** `fix_text()` without an explanation
  fixes the encoding with the default settings whatever it was given; a
  `<` in one line turns HTML unescaping off for every later line; and
  because the check for `"windows-1252"` can never be true, Latin-1 text
  is always tried as strict Windows-1252.

## Compiler gaps

None. `karac fix` made 11 edits for the borrowing rules (`own` on six
parameters, three `.clone()` calls, a `.to_string()` and a `mut ref`
binding), and `karac check` then asked for one more `mut ref` binding of
the same kind. The port then
matched the oracle on every backend.

## Scope

- **No surrogates.** A Kāra string cannot hold a lone surrogate, so
  `fix_surrogates()` always returns its input, as it does in Python for
  text without one.
- **`\N{name}` escapes are not decoded** by `decode_escapes()` (that would
  need the table of character names), and `explain_unicode()`, the CLI,
  `fix_file()` and the display-width helpers are not ported.
- **Codec names are the ones ftfy writes,** without Python's aliases, and
  `apply_plan()` is given the right kind of value for each step.
- **The Unicode data is Python 3.13's** (Unicode 15.1). The all-caps
  entities use Kāra's `to_uppercase()`; the tables job shows they match.

The vendored libraries are unchanged. wcwidth 0.2.13 (MIT) is vendored only
because ftfy imports it.

## Running

```bash
karac run mini_ftfy.kara                  # interpret
karac build --mir mini_ftfy.kara          # native binary
python3 -s -P -B mk.py                    # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out    # regenerate expected.out
python3 -I -B gen_kara.py                 # regenerate the generated tables
```
