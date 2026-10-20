# mini_icalendar

icalendar 7.3.0, the Python library most tools use to read and write
`.ics` files: calendar exports, meeting invitations, task lists,
free/busy replies and CalDAV data. The port covers:
- content lines: unfolding (with icalendar's handling of blank lines
  inside a fold), splitting into name, parameters and value with quoting
  and backslash escapes, the lenient whitespace rules, RFC 6868 caret
  escapes in parameters, and the error messages;
- components: the parser's stack, which components forgive bad lines and
  bad values (VEVENT, and X- properties anywhere), the broken properties
  it keeps, and the second pass when a VTIMEZONE comes after an event
  that uses it;
- value types: text, URIs, calendar addresses, dates, date-times, times,
  durations, periods, date lists, recurrence rules, UTC offsets,
  integers, floats, booleans, GEO, categories and base64 binaries,
  chosen by property name and VALUE parameter as icalendar chooses;
- time zones: TZIDs looked up in the tz database, through Windows zone
  names, by stripping a vendor prefix, or from a VTIMEZONE in an earlier
  calendar;
- writing: icalendar's property order, parameter quoting, escaping and
  folding at 75 octets.

`mk.py` writes `data/`. It holds:
- calendars as real tools write them: a Google Calendar export with a
  recurring standup, a moved instance and an all-day event; an Exchange
  invitation with a Windows-named VTIMEZONE and a base64 attachment; an
  Outlook publish that names Windows zones without defining them; a
  Thunderbird task list with Mozilla-prefixed TZIDs; a CalDAV free/busy
  reply with availability; a journal; a hand-written file with LF line
  ends, lowercase names, a BOM, tab folds and a mismatched END;
- a file of values icalendar keeps as broken, and one with every value
  type;
- a file with two calendars, the first using a zone it defines after
  the event;
- ten broken files, one per fatal error;
- `tz/zones.txt`, the zones `zoneinfo.available_timezones()` knows, each
  marked when icalendar counts it as UTC.

The run writes `data/`, then reads each calendar file and prints its
components with each value and its type, the errors icalendar kept, the
file written back out, and whether reading that again gives the same
text.

`oracle.py` reads the same files through icalendar itself (the copy in
`icalendar/`, unchanged) and wrote `expected.out`. `gen_kara.py` writes
the data section at the end of the program.

The quirks are kept. Some examples:
- `TZID=UTC` is left out when the property is written, but
  `TZID=Etc/UTC` is kept and the time still gets a `Z`.
- A `VALUE=DATE` value with a TZID the library knows becomes midnight
  in that zone, and is written back as a date-time with `VALUE=DATE`
  still on it.
- The time zone cache lives as long as the program, so a Windows zone
  name defined by a VTIMEZONE in one file keeps that definition in the
  next file, which leaves it undefined.
- The END line's name is not checked against the component it closes,
  and the VTIMEZONE cache is filled when the END line says VTIMEZONE,
  whatever it closes.
- In lenient mode a space before the value is kept, so `DTSTART: 2026...`
  is a broken value, while spaces around `;` and `=` in the parameters
  are dropped.
- A line that starts with `;` has no name, so the whole line, with its
  spaces removed, is reported as the bad name.
- An unterminated component is dropped without an error.
- Only VEVENT forgives bad values; a bad DUE in a VTODO stops the whole
  file.

Not ported: the vCard and RFC 9253 value types (`adr`, `n`, `org`,
`inline`, `uid`, `xml-reference`), which a VALUE parameter can ask for.
A period's start and end in two different zones are not compared, since
that needs the zones' offsets. A recurrence rule's UNTIL given as a
period prints differently from the oracle. Python's `\d` and
`str.isdigit` take digits from other scripts; the port takes ASCII
digits only. None of these occur in the data.

License: icalendar is BSD-2-Clause (`LICENSE-icalendar.rst`).
