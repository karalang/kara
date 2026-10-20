# mini_codecs

Python 3.13's codec machinery: `codecs.lookup`, `str.encode`,
`bytes.decode` and `codecs.getincrementaldecoder`, ported from
`Lib/codecs.py`, `Lib/encodings/` and the C codecs in
`Objects/unicodeobject.c` and `Python/codecs.c`.

The port covers:
- `codecs.lookup`: the C side lowers ASCII letters, then
  `encodings.normalize_encoding` collapses punctuation runs to `_`, the
  alias table is tried (also with `.` made `_`), then the name itself, and
  only a codec module may answer;
- the codecs: ascii, latin-1, utf-8, utf-8-sig, utf-16 and utf-32 (with
  a BOM, little-endian and big-endian), unicode_escape,
  raw_unicode_escape, and seven 8-bit charmap codecs (cp1252, cp1251,
  iso8859-15, koi8-r, mac-roman, cp437 and cp850) read from their
  decoding tables;
- every built-in error handler: strict, ignore, replace,
  backslashreplace, xmlcharrefreplace, namereplace, surrogateescape and
  surrogatepass, with the ranges each codec hands them and the message
  each codec raises;
- the incremental decoders, which keep a cut-off character for the next
  piece, learn a utf-16 or utf-32 byte order from the first BOM, and
  wait for three bytes before utf-8-sig decides about its BOM.

Text is a list of code points, as a Python `str` is, so it can hold the
lone surrogates that surrogateescape and surrogatepass make.

`mk.py` writes `data/`:
- the tables the port reads at startup, taken from the Python that runs
  it: each codec module's name, the aliases, each charmap's decoding
  table and the character names namereplace uses;
- `jobs.tsv`: about 200 lookups, decodes, encodes and incremental feeds
  (the format is described at the top of `mk.py`);
- files for four programs:
  - a converter from legacy code pages to UTF-8, driven by a manifest
    that names one file's codec wrongly, lists a missing file and uses a
    name `codecs` does not know;
  - a CSV upload reader that sniffs the byte order mark and falls back
    from UTF-8 to cp1252;
  - a log sanitiser that keeps undecodable bytes through surrogateescape
    and shows them escaped;
  - a mojibake fixer for UTF-8 that was read as cp1252.

`oracle.py` runs the same jobs and programs through Python and wrote
`expected.out`.

The quirks are kept. Some examples:
- `utf-8é` finds the UTF-8 codec, because the normalizer drops letters
  outside ASCII, but `cp-1252` and `utf8-sig` are unknown;
- `b'\xe2\x82\x28'` is an error for two bytes and `b'\xe2\x28\xa1'` for
  one;
- the utf-16 codec reports its errors as `'utf-16-le'` or `'utf-16-be'`;
- the incremental utf-16 decoder refuses a stream without a BOM, which
  the plain decoder reads as little-endian;
- surrogateescape on ascii names only the first character it cannot
  escape, because the C fast path has already written the ones before
  it;
- mojibake that went through `replace` cannot be undone, because U+FFFD
  has no cp1252 byte.

`\N{...}` escapes are not supported, because they need the Unicode
character name database.

It matched on every backend the first time it ran; no gaps were found.
