# pipeline/parcel_sort

A parcel carrier's night sort, the job a hub runs between the evening
pickups and the morning linehaul. For each of four depots (Akron, Boise,
Camden and Dayton) it builds the shippers' manifests and the night's scan
log, writes them as CSV, reads them back and parses them with a loader's
checks, joins the scans to the manifest, rates every parcel, assigns it a
chute, runs five reports, and then loads the outbound parcels of all four
depots onto the linehaul trailers to the national hub.

```
boise_scans.bad line 1734: bad time '2026-10-24 04:10:00'
dayton_manifest.csv line 2: bad parcel id 'D1000000'
...
camden revenue: $153,849.82 on 2,349 parcels (ground $74,389.36, express $42,081.27, overnight $37,379.19); fuel $15,838.76, surcharges $26,861.70; average $65.49; top shipper SH08 $30,822.85
dayton weights: 100 reweighed for $1,268.87 (971.7 kg over declared); 1,096 billed on dim weight; 181 oversize; heaviest D1009156 125.1 kg
akron sort: 22 chutes, 69 bags; busiest G4 291, G9 228, G1 216; EX 41, OS 193, HZ 31
...
TR-0002 1,066 parcels, 12,470.7 kg, 69.8 m3, from A+B; weight full
TR-0004 978 parcels, 11,370.4 kg, 69.8 m3, from B+C; cube full
trailers.log: 8 trailers, 508 bytes, fnv 9dd7d663
```

## What it does

- A depot's manifest has 2,400 parcels from ten shipper accounts, each
  depot with its own mix of shippers. A parcel has a service (ground,
  express or overnight), a declared weight, three dimensions in
  centimetres (small, medium and large boxes), a postcode, a declared
  value and flags for hazmat, fragile and signature on delivery. One
  postcode in 150 has a prefix no one delivers to. Everything comes from
  a 32-bit integer hash of the depot's seed.
- The scan log is what the six induction scanners upload during the
  shift, 20:00 on 23 October to 04:00 on the 24th, sorted by scanner: one
  scan per arrived parcel with what the scale weighed (within 3% of the
  declared weight, or 30% to 99% over it for one parcel in twenty), a
  second scan for one parcel in forty, no scan for one in eighty, and 24
  parcels nobody manifested.
- The parsers check the header and the final newline, the field count,
  the depot's letter and digits of every parcel id, the shipper, the
  service, weights, dimensions, the postcode, the value with two
  decimals, the flags (some of H, F and S in that order), the scanner and
  a time inside the shift, and report `file line N: what`. A copy of the
  Boise scans with one scan at 04:10, and the Dayton manifest read as
  Camden, show two of the errors.
- The join takes the scans in time order, then file order: a parcel's
  first scan is its arrival, a later one a duplicate, and a scan the
  manifest does not list is unmanifested. Manifested parcels never
  scanned did not arrive.
- Rating refuses a parcel with a bad address or a hazmat parcel sent
  overnight (it cannot fly), and sends both to the exception chute.
  Otherwise the zone is 2 plus the distance between the depot's postcode
  prefix and the destination's in steps of 120, at most 8. The billed
  weight is the greater of the declared weight, the scale's weight and
  the dimensional weight (5,000 cm3 to the kilogram), in 500 g units;
  transport is the zone chart's first-unit charge plus its charge per
  further unit, less the shipper's contract discount; fuel is 14.25% of
  transport, rounded half up. Surcharges are $95 for an oversize parcel
  (longest side over 120 cm, or length plus girth over 300 cm), $35 for
  hazmat, $6.50 for a signature and $1.10 for each $100 of declared value
  over $100. A parcel the scale found more than 10% plus 200 g over its
  declared weight is reweighed, and the report shows what that added.
- Chutes: EX for exceptions and unmanifested parcels, OS for oversize,
  HZ for hazmat, `A<zone>` for overnight, `X<zone>` for express and
  `G<first digit of the postcode>` for ground. A bag holds 40 parcels.
- The reports: volume (manifested, arrived, not arrived, unmanifested,
  duplicate scans and the busiest hour); revenue by service with fuel,
  surcharges, the average and the top shipper; weights (reweighs, dim
  weight, oversize, the heaviest parcel); the sort (chutes, bags, the
  three busiest chutes and the special ones); and exceptions, with the
  parcels that missed the 03:30 cutoff.
- The linehaul takes each depot's outbound parcels in scan order onto
  the open trailer, which leaves when the next parcel would take it past
  12,500 kg or 70 m3. Overnight parcels go to the airport, hazmat on its
  own truck, and what was scanned from 03:30 waits for tomorrow. Trailers
  are numbered as they leave and go in `trailers.log`, which is written
  and read back.

Everything is integer arithmetic (cents, grams and cubic centimetres,
division that truncates), so the backends and the Python mirror
`parcel_sort.py` agree by construction; the mirror wrote `expected.out`.

## How the port works

stmt-par: eight groups of statements that do not depend on each other,
in `run()` (`parcel_sort.kara` lines):
- L970-L973 four manifests, each from its depot's seed;
- L976-L979 four scan logs, each over its own manifest, each sorted;
- L982-L989 eight `fs.write` calls, each with the encoder that builds
  its text;
- L1000-L1007 eight reads, each with `?`;
- L1015-L1022 eight parses. Each can fail with `?`, so a parallel run has
  to report the first failure in source order;
- L1035-L1038 four joins, each with its own `Map` of parcel ids and
  `Map` of ids seen;
- L1041-L1044 four ratings, the hot group;
- L1047-L1066 twenty reports, five per depot, that only read what the
  earlier groups made.

The four `load` calls (L1074-L1077) are the case that must stay in
order: each takes `mut lh`, a depot's parcels go onto the trailer the
depots before it left open, and trailers are numbered as they leave.
Loaded in reverse, Dayton fills TR-0001 and TR-0002, five of the seven
full trailers leave for the other reason (weight instead of cube, or
cube instead of weight), and the last one out
carries 845 parcels and 9,877.1 kg from Akron instead of 851 parcels and
9,627.9 kg from Dayton.

## Compiler gaps

None. `karac check` passed the first draft as written, and it matched
the mirror on the first run under the MIR interpreter, the LLVM backend,
the ASan build and the native binary, with nothing on stderr.

## Scope

One night, one hub and one currency; no residential or delivery-area
surcharges, no address correction, no multi-piece shipments, no
re-rating of the parcels held for tomorrow, and no load planning beyond
filling one trailer at a time in scan order.

## Running

```bash
karac run parcel_sort.kara
karac build --mir parcel_sort.kara && ./parcel_sort
python3 -I -B parcel_sort.py > expected.out
```

About 22 s on the MIR interpreter (a debug build of `karac`), 2.5 s on
the LLVM backend and 0.1 s built. It writes its ten files to the
working directory and removes them at the end.
