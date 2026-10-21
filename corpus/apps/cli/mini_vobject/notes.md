# mini_vobject

vobject 0.9.8, the pure-Python reader and writer of vCard and
iCalendar, as the membership secretary of a choral society working
through an address-book import. A vCard or `.ics` file is a flat list
of content lines — `NAME;PARAM=value;PARAM=value:value` — grouped into
`BEGIN`/`END` components, with long lines folded across physical lines
and vCard 2.1's values sometimes quoted-printable. Every exporter
writes them a little differently, and plenty of tools mangle them, so
the reader's job is to unfold, parse, decode and (on the way back out)
re-escape and re-fold each line exactly, and to say precisely where a
broken file breaks.

The port follows vobject's model. `getLogicalLines` unfolds
continuation lines (a line break followed by a space or tab) and
numbers each logical line the way vobject's regex does — a line's
number advances by the physical lines it spanned plus one.
`readComponents` drives a `BEGIN`/`END` stack and raises the same
`ParseError` a malformed file raises (a stray `END`, a mismatched
`END`, an unclosed component, a line with no colon) at the same line.
`parseLine`/`parseParams` split group, name, parameters (merged by
name, with the singleton vCard 2.1 parameters kept apart) and value.
Quoted-printable is decoded after the unfold, so a trailing `=` turns
literal exactly as vobject's does. Each line then takes its behavior:
`N` and `ADR` become a structured `Name`/`Address`, `ORG` and
`CATEGORIES` become lists, text properties are unescaped, and a
base64 `PHOTO` is decoded. `serialize` re-escapes and re-folds each
line at 75 bytes (UTF-8 aware, with `PHOTO` left unwrapped the way
Apple's Address Book wants it) and sorts each component's children the
way its behavior asks.

**Inputs.** `data/` holds 15 files and `jobs.txt`. Seven are good
vCards: an Apple Contacts export (grouped `item1.`/`item2.` lines, a
structured name and address, categories, a base64 photo), an Outlook
batch of three cards in vCard 2.1 with quoted-printable and UTF-8, a
Google Takeout card with two addresses, a card with a deliberately
long folded note, a minimal card, a vCard 2.1 card with singleton
parameters, and two blank-line-separated cards. One is an iCalendar
file (a `VCALENDAR` with a `VEVENT` and a `VTODO`), parsed with
transform off so the generic component model is exercised. The other
seven are broken one way each: no version line, an unclosed card, a
stray `END`, a mismatched `END`, a line with no colon, an empty file,
and a file that is not a vCard at all. `mk.py` writes them.

**Jobs** (56 lines in `jobs.txt`):
- `count`, `parse`: the component and line counts, or the `ParseError`.
- `dump`: every component and line, sorted, with decoded values.
- `fields`: the `FN` and the selected properties of each card.
- `struct`: the structured name, addresses and organisation.
- `roundtrip`: serialize each component, report its length and that it
  reparses, and print every serialized line.
- `qp`: the quoted-printable note and the decoded names.
- `photo`: the base64 photo's encoding and byte length.
- `build`: construct a card programmatically and serialize it.
- `fold`: fold one long line at two widths.
- `escape`: `backslashEscape` on four strings.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the
same jobs through the copy of vobject in `vobject/` (Apache-2.0
licence, in `LICENSE-vobject.txt`). `gen_kara.py` writes the data files
into the Kāra source.

## Edits from the first draft

The port was written against v2's current iterator and ownership
rules: iterating a `Vec` binds each element directly, so the `*`
dereference the older katas use is dropped; a `Str` parameter cannot
be compared to a `String`, so the lookup helpers take `ref String` and
literal call sites pass `"…".to_string()`; mutable parameters write
their mode in the type (`x: mut ref T`); and matching a non-`Copy`
field of a borrowed value is made an explicit `.clone()` so no heap
buffer is freed twice. `karac fix` added the call-site `mut` markers.
Two output bugs were fixed against the oracle: `backslashEscape` must
collapse a CRLF to a single `\n` (vobject replaces `\r\n` before `\n`),
and vobject reports a serialized length in Python `str` characters, not
UTF-8 bytes. After that the output matched `expected.out` on
`__mir-run`, `__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on
the first run.

Written 2026-10-21 by the real-world apps thread; karac at acd3602f55.
