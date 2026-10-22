# mini_pathvalidate

A port of [pathvalidate](https://github.com/thombashi/pathvalidate) 3.3.1
(MIT, Tsuyoshi Hombashi) — the library that validates and sanitizes file
names, used in file-upload handlers and export pipelines to keep
user-supplied names safe to write to disk.

## What it does

`mk.py` writes `data/jobs.tsv`, one `op \t filename` per line (op is
`validate` or `sanitize`). `oracle.py` runs each through
`vendor/pathvalidate/` on the **Windows** platform and prints `expected.out`:
for `validate`, `OK` or the library's own error-reason code and name
(`PV1100 INVALID_CHARACTER`); for `sanitize`, the cleaned name wrapped in
`[ ]` so trailing spaces and empty results stay visible. The filename in the
header has control bytes shown as `\xNN`. `mini_pathvalidate.kara`
reimplements both and must match byte-for-byte on every backend.

Covered (Windows platform, the richest and most-used):

- **validate** in the library's precedence: the null/empty name
  (`NULL_NAME`), absolute paths — drive, UNC and POSIX (`FOUND_ABS_PATH`),
  byte length over 255 (`INVALID_LENGTH`), reserved names — `CON`, `PRN`,
  `AUX`, `CLOCK$`, `NUL`, `COM0..9`, `LPT0..9`, matched on the root and the
  basename, case-insensitively (`RESERVED_NAME`), the invalid-character set —
  control bytes `0..31`/`127` and `/ \ : * ? " < > |` (`INVALID_CHARACTER`),
  and the trailing-space / trailing-period / leading-space rules, with the
  `"."` and `".."` exemptions.
- **sanitize** reproducing the default handlers: strip the invalid
  characters, truncate to 255 bytes, add a trailing `_` to a reserved name
  (replacing it throughout, as the library's `re.sub` does), and — when the
  only fault left is a space/period edge — `lstrip(" ")`, `rstrip(" ")` and
  then `rstrip(" .")` except for `"."`/`".."`. A null/empty input and a name
  that becomes empty after stripping both return `""`.

## Scope

The POSIX and macOS platforms, the human-readable exception message text
(cosmetic and tied to Python's exception formatting — the stable reason code
and name are reported instead), the superscript `COM¹²³`/`LPT¹²³` reserved
variants, and non-ASCII byte-length counting are the boundaries left out.
Inputs here are ASCII, so a character's byte length is one.

## Running

```bash
karac run mini_pathvalidate.kara            # interpret
karac build --mir mini_pathvalidate.kara    # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
```
