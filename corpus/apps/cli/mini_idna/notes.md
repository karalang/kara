# mini_idna

Internationalized domain names as idna 3.20 handles them. requests,
urllib3 and httpx call idna to turn `bücher.example` into
`xn--bcher-kva.example` before they look a host up, and back again for
display. The port covers:
- `encode` and `decode`:
  - strict, or splitting on all four Unicode full stops;
  - with or without UTS #46 mapping, which folds case and compatibility
    forms, drops ignored characters, rejects disallowed ones and can
    apply the STD3 rules;
  - display decoding, which passes a broken A-label through.
- `alabel`, `ulabel` and `check_label`:
  - NFC, the hyphen rules and leading combining marks;
  - the PVALID class, the CONTEXTJ rules for the zero width joiners and
    the CONTEXTO rules for the middle dot, the Greek keraia, Hebrew
    punctuation, the Katakana middle dot and Arabic-Indic digits;
  - the Bidi Rule of RFC 5893;
  - idna's error classes, messages, codes and positions.
- Punycode, as Python's codec writes and reads it, and the check that an
  A-label is the one canonical spelling of its U-label.
- The `idna2008` codec and its incremental encoder and decoder.
- The check requests makes on a host before a request, and the
  `python -m idna` command line.

NFC is written out in the port: canonical decomposition, canonical
ordering and composition, with Hangul syllables done by formula. Two
sets of tables are read from `data/`. idna's own (Unicode 18) give the
UTS #46 mapping, the code point classes, scripts and joining types.
Python's `unicodedata` (Unicode 15.1) gives decompositions, combining
classes, bidirectional classes, categories and names. `mk.py` writes
them for every character the jobs can reach, including what each
A-label in the jobs decodes to.

The run writes `data/` and then works through the 26 jobs in
`data/jobs.jsonl`, 702 operations in all:
- 59 real-looking sites, encoded with UTS #46 and decoded for display:
  German, Russian, Korean, Arabic, Hebrew, Greek, Vietnamese, Thai,
  Hindi, Chinese and Japanese names, full-width letters, ligatures,
  Roman numerals, circled digits, soft hyphens, decomposed accents and
  conjoining Hangul jamo;
- strict IDNA 2008, where emoji, upper case and `_` are refused;
- the label rules, each joiner and context rule, and broken and
  non-canonical A-labels;
- the Bidi Rule on 22 labels, with and without `check_ltr`;
- UTS #46 mapping on 39 strings, with and without STD3;
- `alabel`, `ulabel` and `check_label` on 18 labels each;
- label and domain lengths at the limits, and empty labels;
- the incremental codec, hosts as requests sees them, and 14 command
  lines;
- 15 jobs drawn at random from a fixed seed: domains built from a pool
  of letters, marks, joiners and symbols, random A-labels, and names
  encoded and decoded again.

`oracle.py` runs the same jobs through idna itself and wrote
`expected.out`. It uses the Python files of the 3.20 wheel in `idna/`,
unchanged.

The quirks are kept. Some examples:
- `ß` is a deviation character, so UTS #46 keeps it: `straße.de`
  encodes to `xn--strae-oqa.de`, and `xn--zca.de` decodes to `ß.de`.
- Strict encoding keeps the case of an ASCII label, so `ASCII.Example`
  comes back unchanged, while `Bücher.example` is refused for its `B`.
- An ASCII label that starts with `xn--` is decoded to check it, even on
  the way in. So the incremental encoder, given `xn--` and then `abc`,
  stops at `U+0082`, the first character `abc` decodes to.
- idna's tables are from Unicode 18 and Python's are from Unicode 15.1.
  A character added since, such as U+10D40 in Garay, passes the UTS #46
  mapping but has no bidirectional class, so it fails with
  `bidi_unknown_direction`.
- A failed A-label in display decoding is passed through in lower case,
  but any other failed label still fails the whole name.

Not ported:
- the `transitional` argument, which idna deprecates and ignores, apart
  from its warning;
- bytes input other than the ASCII the codec decodes.

## License

idna is under the BSD 3-Clause license in `LICENSE-idna`:

    Copyright (c) 2013-2026, Kim Davies and contributors.

That covers the files in `idna/`.
