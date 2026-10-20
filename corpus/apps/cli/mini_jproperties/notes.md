# mini_jproperties

jproperties 2.1.2, the Python library for reading and writing Java
`.properties` files (Spring Boot settings, resource bundles,
`gradle.properties`, `log4j.properties`). The port follows the package:
- the parser: a lookahead of one character over a codecs stream reader
  (UTF-8, ISO-8859-1 and ASCII, with Python's decode errors), `#` and `!`
  comments, the `:` and `=` separators and blanks around them, keys and
  values over continued lines, CR, LF and CRLF line ends, the `\t \n \r \f`
  escapes, `\uXXXX` with surrogate pairs, and its parse errors with line
  numbers;
- jproperties' own metadata comments (`#: key=value`, kept for the next
  property) and `metadoc`, which keeps the comments after a property as
  its `_doc`;
- `store`: the initial comments, the timestamp line, the keys in the order
  they were read and then the others sorted, metadata lines, the escapes
  of `_escape_str`, `strict` mode, which writes `\uXXXX` for everything
  that is not printable ASCII when the output is ISO-8859-1, and the
  `jbackslashreplace` handler for what the output encoding cannot hold;
- the mapping API (`p[k] = v`, `del p[k]`, `setmeta`);
- `main()`, the `jproperties` tool: load a file and write it out again,
  changing the encoding.

`mk.py` writes `data/`. It holds:
- files as projects ship them: a Spring Boot `application.properties`, a
  German resource bundle in ISO-8859-1 and in UTF-8, a Japanese one in
  `native2ascii` form, `gradle.properties`, `log4j.properties`, a file
  with metadata comments;
- files for the corners: tab and form feed separators, empty keys and
  values, continued keys and values, CRLF and CR-only lines, a duplicate
  key, a backslash at the end, no newline at the end, an empty file, a
  UTF-8 character cut off at the end, a bad byte;
- `broken/`, one file for each parse error;
- `jobs.tsv`, the jobs.

A `load` job prints each property, its metadata and any error. A `store`
job writes the file out again with the given encoding and mode and prints
the bytes. An `edit` job changes properties and metadata and stores the
result. A `tool` job runs `jproperties` in `data/` and prints its output,
its errors and its exit code; the clock is stopped at 2024-03-09 14:05:07
UTC for the timestamp line. `oracle.py` runs the jobs through the package
itself (`vendor/jproperties.py` and `vendor/six.py`, unchanged) and wrote
`expected.out`. `gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- a blank line ends a continued value, so the next line is a property of
  its own;
- the four characters after `\u` go through Python's `int(s, 16)`, so
  `\u0x41`, `\u 42 ` and `\u4_1a` decode;
- `﻿` decodes to nothing, since the decoder takes it for a byte order
  mark;
- an unknown escape just drops the backslash;
- with escapes in values off, a backslash at the end of a value no longer
  continues it, so the next line becomes a key;
- a key deleted and set again is written after the keys that were read,
  with the new keys, sorted;
- a decode error inside a `\u` escape is reported as a parse error, but
  elsewhere as a `UnicodeDecodeError`;
- a UTF-8 character cut off at the end of the file is dropped without an
  error.

jproperties is under the BSD 3-Clause License, with one function under
the Python Software Foundation License (`LICENSE-jproperties.txt`),
and six under the MIT License (`LICENSE-six.txt`).
