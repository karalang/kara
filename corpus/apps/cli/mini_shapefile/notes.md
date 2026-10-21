# mini_shapefile

pyshp 2.3.1, the pure-Python ESRI shapefile library by Joel Lawhead
and contributors, as the GIS desk of a small county for one afternoon.
The shapefile is still the format most GIS data moves in: a cadastre
of land parcels, the road centrelines, boreholes and soil probes all
arrive as three files side by side, `.shp` for the geometry, `.shx` for
its index and `.dbf` (dBase III) for the attribute table. The format is
old and unforgiving: big- and little-endian fields share one header,
polygon rings carry their meaning in their winding order, the dbf
stores every number as padded text, and files written by other tools
come with upper-case extensions, missing indexes, deleted rows and cut
off records.

The port follows pyshp's model. The Writer builds the three files as
pyshp does, record header first, so a shape or record that fails half
way stays in the file and corrupts what a Reader later sees, just as it
does in Python. Every shape type is there (points, multipoints, lines
and polygons with their M and Z kinds, multipatch, null shapes, and
shapes from GeoJSON dicts with pyshp's rewinding), as are `field()`,
`record()` by position and by keyword, every dbf value conversion with
its `ValueError`, `TypeError` and `ShapefileException`, `autoBalance`
and `balance()`, and `close()` with its checks. The Reader keeps
`load()` with its upper-case fallback, the three headers, `len()` and
`str()`, `shape(i)` through the index or by scanning, `iterShapes` over
the index count or to the end of the file, the m blocks a file may
leave out, records with deleted rows, field subsets, every value parse,
the `_Record` accessors and their errors, the misaligned zip of shapes
and records when a row is deleted, bbox filters, and the `struct.error`
and `UnicodeDecodeError` broken files give. `__geo_interface__` keeps
`organize_polygon_rings` with nested exteriors, orphaned holes and
wrongly wound rings, and the GeoJSON is printed as `json.dumps` prints
it.

**Inputs.** `data/` holds 21 files and `jobs.txt`: twelve layer
scripts (the Ashby Vale cadastre with ponds, an island farm, a
mis-wound allotment and an unclosed ring; road centrelines with
chainage; boreholes; soil probe clusters; a barn multipatch; GeoJSON
features; an afternoon of mistakes; a register with accented field
names; and empty, field-less and unbalanced layers), and shapefiles
other programs left behind (a legacy file with upper-case extensions,
no index and a deleted row, a truncated one, a polyline whose m block
was cut off, point measures with and without m, and dbfs with no
terminator or a short last record). `mk.py` writes them.

**Jobs** (88 lines in `jobs.txt`):
- `write`: run a layer script through the Writer and show the bounding
  boxes, sizes and Adler-32 checksums of the three files it writes to
  `data/out/`.
- `info`, `shapes`, `records`, `fields`, `get`, `shape`, `iter`: read
  a shapefile each way.
- `bbox`: shapes and shape records inside a box.
- `geojson`: the reader's `__geo_interface__` as JSON.
- `extract`: clip the cadastre to a box into a new shapefile.
- `report`: the county report. It checks each parcel's stated area
  against its polygons (holes taken out), places each borehole in its
  parcel, measures the roads and their chainage, and counts the soil
  probes inside each parcel.

The Writer stamps each dbf header with today's date, so `oracle.py`
gives pyshp a clock that always reads 2026-10-21 and the port writes
the same date.

Not ported: reading from zip files and URLs, file-like objects, pyshp's
logging, and the context-manager and `__del__` paths (every layer
closes its Writer itself).

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the
same jobs through the copy of pyshp in `shapefile.py` (MIT licence, in
`LICENSE-pyshp.txt`). `gen_kara.py` writes the data files into the Kāra
source.

## Edits from the first draft

Only the call-site `mut` markers `karac fix` adds. After that, the
output matched `expected.out` on `__mir-run` and `__mir-llvm-run` on
the first run, as did all 39 files written to `data/out/`.

Written 2026-10-21 by the real-world apps thread; karac at 9d1a4c883b.
