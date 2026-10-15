# mini_ini

INI configuration files, read and written the way Python 3.13's
`configparser` does it. Services, build tools, package metadata,
databases, PHP, git and systemd all keep settings in this format. The
program ports the reader, both interpolation styles, the getters and
setters and `write`, and prints exactly what `configparser` prints.

The run writes a dozen real-world files into `data/`:
- an app config with `[DEFAULT]` and `%(name)s` references;
- `setup.cfg`, with multiline values and a blank line inside one;
- `my.cnf`, with valueless keys and `#` inline comments;
- `php.ini`, with `;` inline comments;
- `buildout.cfg`, with `${section:name}` references;
- `.git/config`-style sections such as `[remote "origin"]`, indented with tabs;
- a systemd unit with a repeated key;
- a `defaults.ini` layered under `conf.d/local.ini`.

It also writes broken files: no section header, a duplicate section, a
duplicate option, lines with no delimiter, and a valueless key with a
continuation line. `data/jobs.txt` runs jobs against all of them. Each
job is a parser with its options, then reads, dumps (every raw and
interpolated value), `get`, `getint`, `getboolean`, `items`, `set`,
`add`, `remove-option`, `remove-section`, `has` and `write`.

**The reader** follows `_read_inner`:
- A line whose stripped text starts with a comment prefix is empty.
- An inline prefix starts a comment only at the start of the line or
  after white space.
- A blank line is kept inside a value, or ends the value when
  `empty_lines_in_values` is off.
- A line indented deeper than its option's line continues that option.
- A header runs from `[` to the last `]`.
- An option ends at the first delimiter, and its name is lower-cased.
- `MissingSectionHeaderError`, `DuplicateSectionError`,
  `DuplicateOptionError` and `MultilineContinuationError` stop the
  read. Lines with no delimiter are collected and raised together as a
  `ParsingError` at the end of the file, after the rest of it has been
  read.

**Interpolation**:
- Basic: `%(name)s` and `%%`.
- Extended: `${name}`, `${section:name}` and `$$`.
- Both recurse up to ten levels, and the value-substitution, syntax and
  depth errors carry Python's messages.
- `set` rejects values that would not interpolate.

Not supported: `vars` and `fallback` arguments, converters, the
`allow_unnamed_section` mode, `read_string` and `read_dict`,
`space_around_delimiters=False`, and `LegacyInterpolation`.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs
`data/jobs.txt` with Python 3.13's `configparser`. Run it from this
directory after `mk.py` writes `data/`.

## Edits from mini_ini.orig.kara to mini_ini.kara

Two edits:
- **For an existing row.** `karac check` refuses the chained field
  receivers `self.defaults.vals[i]` and `self.defaults.keys.len()` with
  the legacy codegen's `chained_field_receiver` error
  (B-2026-10-14-70). They are now the methods `Section.val_at` and
  `Section.len`.
- **For a warning.** The two `Vec.remove` results in `Section.remove`
  are discarded with `let _ =`.

All three MIR surfaces run the original.

Written 2026-10-15 by the real-world apps thread; karac at 35fcd8197c.
