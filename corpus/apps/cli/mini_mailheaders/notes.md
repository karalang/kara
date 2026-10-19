# mini_mailheaders

The header half of Python 3.13's `email` package, ported from
`email/utils.py`, `email/_parseaddr.py`, `email/header.py` and the parts
of `email/charset.py`, `quoprimime` and `base64mime` they use. It parses
and formats addresses (`parseaddr`, `getaddresses`, `formataddr`,
`quote`, `unquote`), dates (`parsedate_tz`, `parsedate`,
`parsedate_to_datetime`, `mktime_tz`, `formatdate`) and RFC 2231
parameters, and it decodes and encodes RFC 2047 encoded words
(`decode_header`, `make_header`, `Header`). The codecs it needs are
ported too: UTF-8 with CPython's error messages, Latin-1, Latin-9,
Windows-1252, ASCII and `raw-unicode-escape`.

The run writes 11 messages under `data/mail/` and 507 jobs to
`data/jobs.jsonl`. Each job is printed after `== ` and the job. The jobs
call the functions above, in strict and non-strict mode where both
exist; `header` builds a `Header` and encodes it with a given line
length and split characters, and `header_parts` appends chunks in
several charsets. Then the run reads the mailbox and prints a report
for each message: sender, recipients, date and its UTC timestamp, the
decoded subject, and the headers of a reply. An address book of every
address seen comes last.

`oracle.py` runs the same jobs and the same report through the real
`email.utils` and `email.header` (CPython 3.13), and wrote
`expected.out`.

The library's quirks are kept. Some examples:
- `parseaddr("Smith, Alice <alice@example.com>")` is `('', '')` in
  strict mode, because the unquoted comma makes two addresses. In
  non-strict mode it is `('', 'Smith')`. `getaddresses` returns both
  halves, `('', 'Smith')` and `('Alice', 'alice@example.com')`.
- Strict mode refuses `a@b@c.example` and a domain literal such as
  `trent@[192.0.2.1]`, and returns `('', '')` for them. An unclosed
  comment in non-strict mode swallows the rest of the line into the
  name.
- `formataddr` quotes a name with `.`, `@`, `[` or `,` in it, and
  encodes a non-ASCII name: in UTF-8 as base64 or Q, whichever is
  shorter, and in Latin-1 always as Q. A non-ASCII local part raises
  `UnicodeEncodeError`.
- `parsedate_tz` takes a zone it does not know, such as `CEST`, as
  offset 0, and so it takes `+05:30`. It accepts hour 25 and second 60,
  and it reads two-digit years 69 to 99 as 19xx and 00 to 68 as 20xx.
- `make_header` on an encoded word with an empty charset prints
  `nocharset`, but encoding it raises `LookupError: unknown encoding:`,
  because one path treats the empty codec name as false and the other
  does not.
- An encoded word in an unknown charset decodes to raw bytes, and the
  subject report then fails with `LookupError`; the reply still encodes
  the subject again as UTF-8.

Limits of the port:
- Only the six codecs above are known. Any other charset name is a
  `LookupError`, where Python knows many more.
- `formatdate` and `parsedate_to_datetime` work in UTC and fixed
  offsets only, never in the local time zone.

Gaps found while writing it (rows in the kara repo):
- B-2026-10-19-4: a literal-only nested `if` arm is not coerced to
  `Str` when another arm is `Str`. The tuple in `decode_bytes` carries
  a type annotation instead.
- B-2026-10-19-5: `Vec.join` refuses a `Str` separator, though the
  library docs give `sep: Str`. `finish` passes a `String`.
- B-2026-10-19-6: MIR borrow checking keeps a `Vec[Str]` from `split`
  borrowing its source until the end of the scope, so a later
  assignment to the source is refused; `karac check` accepts it.
  `parsedate_tz_raw` builds the owned parts in a block first.

Written 2026-10-19 by the real-world apps thread; karac at 3934e06a45.
