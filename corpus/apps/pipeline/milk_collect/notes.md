# pipeline/milk_collect

A week of milk collection at a dairy co-operative's four plants: valley
(VAL), hill (HIL), coast (COA) and lake (LAK). For each plant it builds
the farm register, the tanker fleet on each route, the week's pickups at
the farms and the lab's results for each pickup's sample, writes them as
CSV, reads them back and parses them with the plant's checks, and closes
the week day by day: the tankers collect along their routes, each load's
samples are read at the plant, a load with antibiotics in it is dumped
and charged to the farms that caused it, and the plant processes what it
can and stores the rest. The farms are paid for their fat and protein
with quality bonuses and penalties. It writes and reads back the loads
and the payments, runs five reports per plant, and then the co-op's
drier takes the plants' surplus in the order they send it.

```
VAL farms: 120 rows, 2,992 bytes, fnv a8f0932e
...
VAL_pickups.bad line 41: no farm 1999
HIL_samples.bad line 31: bad inhibitor result 'X'
COA_tankers.csv line 2: tanker T3-01 is not at LAK
parsed files match the built ones: true
...
VAL collections: 607 pickups, 2,585,669 litres, 20 refused (12 warm, 8 suspended, 83,927 litres); by day 88 87 86 86 85 88 87
...
VAL quality: fat 4.19%, protein 3.34%, cells 301k; farms by cell count 34 under 200k, 58 200k-399k, 22 400k-499k, 6 500k and over; 22 pickups over 100k bacteria
...
VAL loads: 115 tanker loads on 4 routes, 54 at least 90% full; 5 dumped (138,260 litres) for inhibitors from 5 farms, charged $62,217.00; busiest route 4 with 35 loads
...
COA intake: received 3,067,082 litres, processed 2,963,285, 70,000 in the silo at the end, 33,797 surplus; 6 days at capacity; by day 413,285 425,000 425,000 425,000 425,000 425,000 425,000
...
LAK payments: 70 farms, $854,257.93 paid, 48.23 c/L; quality bonus $42,166.64, penalties $23,056.73, over quota $5,996.28; 3 farms owe; top farm 4022 $28,070.28
...
COA drier: sent 33,797 litres of surplus, 23,752 dried, 10,045 sold on the spot market; 0 litres of room left
LAK drier: sent 72,535 litres of surplus, 0 dried, 72,535 sold on the spot market; 0 litres of room left
drier.log: 4 entries, 77 bytes, fnv 64ebc35c
```

## What it does

- **The farms**: 120, 90, 140 and 70, each on a route, with a herd of 30
  to 229 cows, a bulk tank sized for what it holds between calls, a
  tanker every day (herds of 120 or more) or every other day, an organic
  flag on about one in eight, and a weekly quota of 25 litres a cow a
  day.
- **The fleet**: one to three tankers per route, of 18,000, 24,000 or
  30,000 litres.
- **The pickups**: each farm due that day, in the order the route
  reaches it from 05:00, with the litres in its tank and the tank's
  temperature (about one in fifty is too warm).
- **The lab results**: one row per pickup, with fat and protein (in
  hundredths of a percent), somatic cells (thousands per mL), bacteria
  (thousand cfu per mL, high on about one in twenty-five) and an
  inhibitor test, positive on about one in 170.
- **The parsers** check the header and final newline, the field count,
  farm ids (the plant's, once each), routes, herds, tanks, the pickup
  interval, the organic flag and the quota; tanker ids (the plant's,
  once each), routes and capacities, and a tanker on every route; pickup
  numbers in order, days, `HH:MM` times that never go backwards, farms
  that exist, litres no more than the farm's tank and temperatures; and
  one lab row per pickup with `P` or `N` for the inhibitor test. They
  report `file line N: what`. A copy of VAL's pickups naming a farm it
  does not have, a copy of HIL's lab results with an inhibitor result
  that is neither P nor N, and COA's tankers read as LAK's show three of
  them.
- **The week's close** runs day by day. A pickup is refused if the farm
  is suspended or its tank is warmer than 6.0 °C. Otherwise it goes into
  the route's open tanker load; a load that would overflow goes to the
  plant and the route's next tanker, in turn, starts a new one. What is
  still open at the end of the day goes in too. At the plant, a load
  with any positive inhibitor sample is dumped: it is charged at 45 cents
  a litre, split between the farms whose samples were positive, those
  farms are not collected for the next two days, and the other farms on
  the load are still paid for their milk. The plant processes up to its
  capacity a day; what is left goes in the silo, and what the silo cannot
  hold is surplus.
- **The payments**: fat and protein by weight (1.03 kg a litre) at a
  price per kg, a cell count band of +2 to -8 cents a litre, a 2 cent
  penalty for high bacteria, a 12 cent organic premium, 4 cents a litre
  haulage, the dumping charges, and 6 cents a litre over the week's
  quota. A farm with a dumping charge can owe the co-op.
- **The loads and the payments** are written per plant and read back.
- **The reports**: collections (pickups, litres, refusals by reason and
  pickups by day); quality (fat, protein and cells weighted by litres,
  farms by cell count band and pickups with high bacteria); loads (loads
  per route, the share at least 90% full, loads dumped, farms charged and
  the busiest route); intake (received, processed, the silo at the end,
  surplus and days at capacity); and payments (the total, cents a litre,
  bonuses, penalties, over-quota deductions, farms that owe and the top
  farm).
- **The drier** has room for 150,000 litres in the week. Each plant
  sends its surplus, and what the drier has no room for is sold on the
  spot market. Every send goes in `drier.log`, which is written and read
  back.

Everything is integer arithmetic over litres, cents, minutes and days,
so the backends and the Python mirror `milk_collect.py` agree by
construction; the mirror wrote `expected.out`.

## How the port works

stmt-par: eight groups of statements that do not depend on each other,
in `run()` (`milk_collect.kara` lines):
- L952-L955 four farm registers and L956-L959 four fleets, then
  L960-L963 four pickup builds over each plant's own farms and L964-L967
  four sets of lab results over its pickups;
- L970-L985 sixteen `fs.write` calls, each with the encoder that builds
  its text;
- L1003-L1018 sixteen reads, each with `?`;
- L1040-L1047 eight register and fleet parses, then L1048-L1051 four
  pickup parses, which need the register, then L1052-L1055 four lab
  result parses, which need the pickups. Each can fail with `?`, so a
  parallel run has to report the first failure in source order;
- L1073-L1076 four week closes, the hot group: each runs its plant's
  week over data it alone owns;
- L1079-L1086 eight load and payment writes and L1090-L1097 their reads;
- L1108-L1127 twenty reports, five per plant, that only read what the
  earlier groups made.

The four `send` calls (L1135-L1138) are the case that must stay in
order: each takes `mut drier`, and a plant gets only the room the plants
before it left. In order, VAL and HIL are dried in full and COA gets
23,752 of its 33,797 litres; sent in reverse, LAK and COA are dried in
full, HIL gets 43,668 of its 90,839 and VAL none.

## Edits from milk_collect.orig.kara to milk_collect.kara

One. `karac check` raised one error, a move of `acc[k]` out of the
accounts vector in `pay`, and `karac fix` added `#[derive(Copy)]` to
`Account`, whose fields are all `i64`. Then the port matched the mirror
on the first run under the MIR interpreter, the LLVM backend, the ASan
build, the native binary and `karac run`, with nothing on stderr. The
files it writes match the mirror's byte for byte.

## Scope

One week at four plants with no milk moved between plants, no tanker
breakdowns or missed calls, no rejected loads other than for
inhibitors, no component testing beyond the one sample per pickup, no
seasonal or volume pricing, no retests of a dumped load, and nothing
after the week.

## Running

```bash
karac run milk_collect.kara
karac build --mir milk_collect.kara && ./milk_collect
python3 -I -B milk_collect.py > expected.out
MILK_REVERSE=1 python3 -I -B milk_collect.py   # the plants send to the drier in reverse
```

About 2.6 s on the MIR interpreter (a debug build of `karac`), 3.7 s on
the LLVM backend and 0.01 s built. It writes its 27 files to the
working directory and removes them at the end.
