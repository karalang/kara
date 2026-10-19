# mini_gpxpy

gpxpy 1.6.2, the Python library for GPS tracks in GPX format, and its
`gpxinfo` command. Strava exports, Garmin watches, OpenStreetMap
traces and most hiking apps write GPX; gpxpy is what Python tools read
it with. The port covers:
- an ElementTree-style XML parser: namespaces resolved to `{uri}local`,
  entity and character references, CDATA, comments and processing
  instructions, and expat's error messages with line and column;
- gpxpy's field lists for GPX 1.0 and 1.1, which drive both reading and
  writing: attributes, nested `metadata`/`author`/`copyright`/`link`
  groups that are left out when empty, e-mail split into `id` and
  `domain`, extensions kept as elements and written back with their
  namespace prefixes, `fix` checked against its allowed values;
- its timestamp regex with the regex's backtracking over one- and
  two-digit fields, fractions cut to microseconds, and its `SimpleTZ`
  offsets (`Z`, `+05:30`, `+0530`, `+05`, a Unicode minus);
- the geo module: flat-earth and haversine distances, rhumb-line and
  great-circle courses, Ramer-Douglas-Peucker simplification, moving
  a point by a distance and an angle;
- track statistics: 2D and 3D length, moving and stopped time, the
  max speed with outlier distances and the top 5% of speeds dropped,
  uphill and downhill on smoothed elevations, bounds, duration, the
  nearest point and the number of passes near a location, the location
  at a time;
- track edits: simplify, reduce to a minimum distance or a number of
  points, smooth (with extremes removed), fill in missing elevations,
  times and speeds along the distance, split and join segments, move,
  shift the times, and fill a track without times at regular intervals.

`mk.py` writes `data/`: gpxpy's own test files from `tracks/` (the six
longest cut to their first points, so the interpreted run stays
short), a run recorded by a sports watch (Garmin's heart rate and
cadence extensions, local times with milliseconds, a pause and a GPS
dropout that starts a new segment), a file of awkward timestamps and
numbers, and four broken files.

The run writes `data/` and, for each file, prints gpxinfo's report
(the GPX and each segment; in seconds and miles too for two of them),
the XML gpxpy writes for it with a CRC-32 (in full when it is short),
whether parsing that XML and writing it again gives the same text, the
XML in the other GPX version and without pretty printing, and the
results of the track operations.

`oracle.py` runs the same through gpxpy itself (the copy in `gpxpy/`,
unchanged, over the standard library's ElementTree, not lxml) and
`gpxinfo.py` (gpxpy's script, unchanged), and wrote `expected.out`.
`gen_kara.py` writes the data section at the end of the program.

The quirks are kept. Some examples:
- A segment has no bounds when any bound is 0.0: gpxpy tests the
  floats for truth.
- `to_xml()` stores the schema locations it writes, so `to_xml('1.1')`
  on a 1.0 file still points at the 1.0 schema.
- Sums that start from the int 0 print as `0` when nothing was added
  (lengths, uphill and downhill), and an elevation angle between two
  points at the same place is the int 0.
- Removing extremes compares an elevation change against the 2D
  distance threshold.
- A timestamp gpxpy cannot parse (month 13, 29 February 2021, a space
  before it) is dropped without an error, and a letter where the zone
  sign belongs is accepted, because the sign is a character range from
  `+` to the Unicode minus.
- Python reads the files with universal newlines, so the port turns
  CRLF into LF before parsing.

License: gpxpy is Apache 2.0 (`LICENSE-gpxpy`, `NOTICE-gpxpy`); its test
files in `tracks/` come with it.
