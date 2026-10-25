# pipeline/grain_intake

A farmers' grain co-op's harvest week at four country elevators, Ashby,
Belmont, Clare and Dunmore. It builds each elevator's growers, its bins,
the forward contracts its growers sold and the week's scale tickets,
writes them as CSV, reads them back and parses them with the scale
house's checks, closes the week (docking, shrink, grades, bins and
settlements), writes each elevator's settlements and grower statements,
runs five reports per elevator, and then hands out the railroad's hopper
cars for the week in the order the elevators ask.

```
ASH growers: 80 rows, 1,560 bytes, fnv bde45566
ASH tickets: 420 rows, 22,682 bytes, fnv b000d265
...
ASH_tickets.bad line 41: ticket 101040 has tare 90,000 lb over gross 80,422 lb
CLR_contracts.bad line 31: no grower '3999'
DUN_bins.csv line 2: bin DUN-B01 is not at BEL
...
CLR intake: 420 tickets, 400 taken, 20 too wet, 0 turned away for room; corn 208,598.97 bu at 22.2%, soy 60,530.29 bu at 14.0%, wheat 27,543.50 bu at 14.4%
ASH grades: CORN 1:19 2:74 3:71 4:96 5:1; SOY 2:15 3:62 4:17 5:1; WHEAT 1:1 2:4 3:21 4:12; worst damage ticket 101338 (SOY, 11.2%)
...
ASH settlement: $1,598,397.48 value, $57,779.42 drying, $11,256.69 discounts, $1,300.28 fees, $6,059.92 checkoff, $1,522,001.17 paid; 88,561.25 bu on contract at $5.27, 204,944.66 bu at spot at $5.51
...
CLR rail: asked 42 cars, granted 14, 49,000.00 bu shipped, 94,781.04 bu held; 0 cars left
DUN rail: asked 28 cars, granted 0, 0.00 bu shipped, 95,357.90 bu held; 0 cars left
rail.log: 4 entries, 115 bytes, fnv d3a61b56
```

## What it does

- **The growers**: 80 at each elevator, with a number that carries the
  elevator, and whether they are members of the co-op (most are).
- **The bins**: five for corn, three for soybeans and two for wheat, each
  holding 20,000 to 50,000 bushels and up to 29% full before the week.
- **The contracts**: zero to three for each grower, each for a crop, 500
  to 2,500 bushels at a price near the first day's cash price, and the
  days it may be delivered on.
- **The tickets**: 420 a week in day order, each a grower's truck (a
  semi, or a straight truck) with its gross and tare weights and the
  probe sample's moisture, test weight, foreign material and damage. The
  mix of crops and how wet the corn comes in differ by elevator.
- **The parsers** check the header and final newline, the field count,
  grower numbers (four digits, the elevator's, once each), names and the
  member flag; bin names (the elevator's, once each), crops, capacities
  and a start no more than the capacity; contract numbers, that each
  names a grower, bushels, a price with two decimals and a delivery
  window inside the week; and ticket numbers in order, growers, trucks,
  days, a tare under the gross, and the sample's readings with one
  decimal and in range. They report `file line N: what`. A copy of ASH's
  tickets with a tare over the gross, a copy of CLR's contracts naming a
  grower it does not have, and DUN's bins read as BEL's show three of
  them.
- **The week's close** takes each ticket in order. Foreign material over
  the crop's limit is docked from the net weight; moisture over the base
  (15.0% for corn, 13.0% for soybeans, 13.5% for wheat) shrinks it by
  1.4 or 1.3 points a point plus half a point of handling shrink, and the
  dry weight gives the bushels (56 pounds for corn, 60 for the others).
  Grain over the most the co-op takes is turned away as too wet. The
  grade is the first US grade whose test weight, damage and foreign
  material limits the sample meets, or sample grade. Each ticket goes in
  the first bin of its crop with room; corn with no room goes on the
  ground pile, and soybeans or wheat with no room are turned away. The
  bushels fill the grower's open contracts for the crop whose window has
  the day, in contract order, and the rest sell at the day's board price
  less the elevator's basis. The pay is that value less drying (by the
  half point over the base), the test weight, damage and sample grade
  discounts, a handling fee for those who are not members, and the
  checkoff (a cent a bushel on corn, two on wheat, half a percent of the
  value on soybeans).
- **The settlements and grower statements** are written per elevator and
  read back.
- **The reports**: intake (tickets taken, too wet and turned away, and
  bushels and mean moisture by crop); grades by crop and the worst
  damage; each bin's fill and the ground pile; the settlement totals and
  the average contract and spot prices; and the contracts filled, partly
  filled and untouched, the bushels still owed and the three growers paid
  the most.
- **The rail cars**: each elevator ships its ground pile and its
  soybeans and asks for the 3,500-bushel hopper cars that takes. The
  railroad has 70 for the week and grants what it has left. Every request
  goes in `rail.log`, which is written and read back.

Everything is integer arithmetic in pounds, tenths of a percent or a
pound, hundredths of a bushel and cents, so the backends and the Python
mirror `grain_intake.py` agree by construction; the mirror wrote
`expected.out`.

## How the port works

stmt-par: eight groups of statements that do not depend on each other,
in `run()` (`grain_intake.kara` lines):
- L1046-L1049 four grower builds and L1050-L1053 four bin builds, then
  L1054-L1057 four contract builds and L1058-L1061 four ticket builds,
  each over its own elevator's growers;
- L1064-L1079 sixteen `fs.write` calls, each with the encoder that builds
  its text;
- L1096-L1111 sixteen reads, each with `?`;
- L1132-L1135 four grower parses and L1136-L1139 four bin parses, then
  L1140-L1143 four contract parses and L1144-L1147 four ticket parses,
  which read the growers. Each can fail with `?`, so a parallel run has
  to report the first failure in source order;
- L1164-L1167 four week closes, the hot group: each grades, bins and
  settles its elevator's tickets over data it alone owns;
- L1170-L1177 eight settlement and statement writes and L1181-L1188
  their reads;
- L1199-L1218 twenty reports, five per elevator, that only read what the
  earlier groups made.

The four `ask_rail` calls (L1226-L1229) are the case that must stay in
order: each takes `mut rail`, and an elevator gets only the cars the
elevators before it left. In order, ASH and BEL get all they ask for,
CLR gets 14 of its 42 and DUN gets none; asked in reverse, DUN and CLR
get all they ask for and BEL and ASH get none.

## Edits from grain_intake.orig.kara to grain_intake.kara

None. `karac check` passed on the first draft, and it matched the mirror
on the first run under the MIR interpreter, the LLVM backend, the ASan
build and the native binary, with nothing on stderr.

## Scope

One harvest week at four elevators, with no grain bank or deferred
pricing, no basis or hedge-to-arrive contracts, no storage charges, no
blending of bins, no trucking out by road, no weather, no splits between
landlord and tenant, and no rail cars beyond the week's allotment.

## Running

```bash
karac run grain_intake.kara
karac build --mir grain_intake.kara && ./grain_intake
python3 -I -B grain_intake.py > expected.out
GRAIN_REVERSE=1 python3 -I -B grain_intake.py   # the elevators ask for cars in reverse
```

About 4 s on the MIR interpreter (a debug build of `karac`), 3.5 s on
the LLVM backend and under 0.01 s built. It writes its 27 files to the
working directory and removes them at the end.
