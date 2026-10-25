# pipeline/blood_bank

A week at a regional blood service's four donation centres: north (NOR),
south (SOU), east (EAS) and west (WES). For each centre it builds the
donor registry, the opening stock, the week's donor visits, the lab's
screening results and the hospitals' orders, writes them as CSV, reads
them back and parses them with the centre's checks, and closes the week
day by day: donations are checked and collected, screened and split
into components, and the hospitals' orders are filled from the shelf.
It writes and reads back the issues and the closing inventory, runs five
reports per centre, and then the national reserve of O-negative red
cells covers the centres' shortfalls in the order they ask.

```
NOR donors: 2,400 rows, 63,150 bytes, fnv a6ce83ad
...
NOR_visits.bad line 41: no donor 199999
SOU_tests.bad line 31: bad HIV result 'X'
EAS_orders.csv line 2: hospital EAS-H2 is not at WES
parsed files match the built ones: true
...
NOR collections: 493 visits, 350 donations (287 whole blood, 63 apheresis), 143 deferred (49 too soon, 47 low hemoglobin, 23 age, 12 weight, 12 earlier deferral); by day 45 56 47 53 47 51 51
...
NOR testing: 350 tested, 6 reactive (HBV 2, HCV 1, HIV 0, syphilis 3), 2 failed ABO confirmation; 6 donors deferred for good, 22 components discarded
...
SOU components: 353 red cells, 353 plasma, 70 platelet pools from 353 buffy coats (73 unpooled), 146 apheresis platelets; 56 expired (red cells 0, platelets 56, plasma 0)
...
NOR hospitals: 184 orders for 475 units, 449 issued (29 substituted, 3 Rh override), 26 short (red cells 26, platelets 0, plasma 0); NOR-H1 153/159, NOR-H2 184/196, NOR-H3 112/120
...
EAS reserve: asked for 14 O- red cells, sent 4, 10 not covered; 0 left in the reserve
WES reserve: asked for 0 O- red cells, sent 0, 0 not covered; 0 left in the reserve
reserve.log: 4 entries, 46 bytes, fnv 08860e08
```

## What it does

- **The donors**: 2,400, 2,900, 1,800 and 2,100, each with a blood group
  (O-, O+, A-, A+, B-, B+, AB-, AB+, at the shares a European population
  has), a birth year, a weight, the days of the last whole blood and
  apheresis donations before the week, and sometimes a deferral that
  runs into the week.
- **The opening stock**: 260, 300, 200 and 220 units of red cells,
  platelets and plasma, each collected some days before the week and
  still inside its shelf life (42 days, 5 and 365).
- **The visits**: 55 to 85 a day, each a donor coming in for whole blood
  or apheresis at a time of day, with a hemoglobin reading.
- **The lab results**: one row per visit, reactive or not for HBV, HCV,
  HIV and syphilis, and the group the lab typed.
- **The orders**: 21 to 30 a day from two to four hospitals, each for a
  component, a patient group and a number of units, some urgent.
- **The parsers** check the header and final newline, the field count,
  donor ids (the centre's, once each), groups, birth years, weights,
  days and deferrals; unit ids (the centre's, once each), components,
  groups and units that expired before the week; visit numbers in
  order, donors that exist, `HH:MM` times that never go backwards,
  `WB` or `APH` and hemoglobin; one lab row per visit with `R` or `N`
  per marker; and order numbers in order, the centre's hospitals,
  components, units and priority. They report `file line N: what`. A
  copy of NOR's visits naming a donor it does not have, a copy of SOU's
  lab results with an HIV result that is neither R nor N, and EAS's
  orders read as WES's show three of them.
- **The week's close** runs day by day. Units past their shelf life
  leave the shelf, and yesterday's tested units join it. Each visit is
  checked in turn: an earlier deferral still running, age (18 to 70),
  weight (50 kg), hemoglobin (125 g/L), and the interval since the last
  donation (56 days between whole blood donations, 7 after apheresis,
  28 from whole blood to apheresis). A donor who passes gives. A
  donation with any reactive screen is discarded and the donor deferred
  for good; one whose typed group is not the donor's group is
  discarded. Whole blood gives red cells, plasma and a buffy coat; four
  buffy coats of a group pool into a platelet unit at the end of the
  day; apheresis gives one platelet unit, or two from a donor of 70 kg
  or more. Then the day's orders are filled, urgent first: each unit is
  the compatible one on the shelf that is the patient's own group if it
  can be, then the one that can serve the fewest other groups (for
  platelets, the right Rh), then the one expiring first. An urgent
  red cell order for an Rh-negative patient with no Rh-negative unit
  left takes an Rh-positive one; what cannot be filled is short.
- **The issues and the closing inventory** are written per centre and
  read back.
- **The reports**: collections (visits, donations by kind, deferrals by
  reason and donations by day); testing (reactive donations by marker,
  ABO failures, donors deferred for good and components discarded);
  components (made by kind, platelet pools and unpooled buffy coats, and
  expired units); hospitals (orders, units asked and issued, substitutes
  and Rh overrides, units short by component and each hospital's share);
  and inventory (red cells by group, platelets, plasma and what expires
  within three days).
- **The reserve** holds 45 O-negative red cell units for the week. Each
  centre asks for its red cell shortfall, and the reserve sends what it
  has left. Every request goes in `reserve.log`, which is written and
  read back.

Everything is integer arithmetic over days, minutes and units, so the
backends and the Python mirror `blood_bank.py` agree by construction;
the mirror wrote `expected.out`.

## How the port works

stmt-par: seven groups of statements that do not depend on each other,
in `run()` (`blood_bank.kara` lines):
- L1162-L1165 four registry builds, L1166-L1169 four stock builds and
  L1170-L1173 four order builds, then L1174-L1177 four visit builds over
  each centre's own donors and L1178-L1181 four sets of lab results over
  its visits;
- L1184-L1203 twenty `fs.write` calls, each with the encoder that builds
  its text;
- L1221-L1240 twenty reads, each with `?`;
- L1266-L1277 twelve registry, stock and order parses, then L1278-L1281
  four visit parses, which need the registry, then L1282-L1285 four lab
  result parses, which need the visits. Each can fail with `?`, so a
  parallel run has to report the first failure in source order;
- L1304-L1307 four week closes, the hot group: each runs its centre's
  week over data it alone owns;
- L1310-L1317 eight issue and inventory writes and L1321-L1328 their
  reads;
- L1339-L1358 twenty reports, five per centre, that only read what the
  earlier groups made.

The four `draw` calls (L1366-L1369) are the case that must stay in
order: each takes `mut reserve`, and a centre is sent only what the
centres before it left. In order, NOR and SOU are covered and EAS gets 4
of its 14; asked in reverse, WES, EAS and SOU are covered and NOR gets
16 of its 26.

## Edits from blood_bank.orig.kara to blood_bank.kara

None. `karac check` raised nothing, and the port matched the mirror on
the first run under the MIR interpreter, the LLVM backend, the ASan
build and the native binary, with nothing on stderr.

## Scope

One week at four centres with no units moved between centres, no
crossmatching, no irradiated, washed or CMV-negative units, no
autologous or directed donations, no recall of units already issued, no
returns from the hospitals, and nothing after the week.

## Running

```bash
karac run blood_bank.kara
karac build --mir blood_bank.kara && ./blood_bank
python3 -I -B blood_bank.py > expected.out
BLOOD_REVERSE=1 python3 -I -B blood_bank.py   # the centres draw on the reserve in reverse
```

About 6 s on the MIR interpreter (a debug build of `karac`), 5.5 s on
the LLVM backend and 0.03 s built. It writes its 31 files to the
working directory and removes them at the end.
