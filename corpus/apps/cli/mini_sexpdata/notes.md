# mini_sexpdata

sexpdata 1.0.2, the Python library that reads and writes S-expressions,
as tools use it for KiCad files, Emacs Lisp data, dune files and
SMT-LIB. The port follows the package:
- `parse` and `loads` with every option (`nil`, `true`, `false`,
  `string_to`, `line_comment`): the reader's string and atom scanners,
  String's and Symbol's escapes, quotes that take the next expression,
  `[...]` as Brackets, and atoms tried as Python's `int()` and `float()`
  would take them (signs, underscores, `inf` and `nan`, digits from any
  script, non-ASCII white space at the ends, big integers);
- `dumps` with `pretty_print`, `indent_as`, `str_as`, `true_as`,
  `false_as`, `none_as` and `tuple_as`: lists, dicts as plists,
  Brackets, Quoted, Symbol and String quoting, and Python's float
  `str()`;
- `car` and `cdr`, including the dotted-pair shortcut;
- the errors with their messages: too few or too many closing brackets,
  nothing after a quote, and the `AttributeError`, `IndexError` and
  `AssertionError` sexpdata lets through.

`mk.py` writes `data/`. It holds:
- files as the tools that use S-expressions write them: a KiCad 8
  schematic and footprint library table, a dune file and dune-project,
  an Emacs package archive index and `custom-set-variables`, an SMT-LIB
  query and a Guix-style manifest;
- 15 files for the corners of the reader and 13 broken files;
- seven JSON values to dump, and `jobs.tsv`, the jobs.

`parse`, `loads`, `roundtrip`, `dumps` and `carcdr` jobs print what
sexpdata gives. Six programs work on the trees: a KiCad bill of
materials, the footprint libraries with `${VAR}` expanded, dune's
libraries and dependencies, the Emacs packages with their requirements,
the custom settings, and the SMT-LIB commands. `oracle.py` runs the jobs
through the package itself (`vendor/sexpdata.py`, unchanged) and wrote
`expected.out`. `gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- `#t` dumps as `\#t`, since `#` is one of the characters Symbol quotes;
- `nil` reads as an empty list, which dumps as `()`;
- a symbol whose text has `\q` in it keeps the backslash;
- `str_as='symbol'` writes a string's text with nothing quoted;
- a quote parses the rest of its list and keeps only the first
  expression, so `'` before a comment quotes what comes after it;
- `loads` of a file with two expressions, or none, is an
  `AssertionError` with no message.

sexpdata is under the BSD 2-Clause License (`LICENSE-sexpdata.txt`).
