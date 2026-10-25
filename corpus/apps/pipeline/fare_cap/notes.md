# pipeline/fare_cap

A week of smart-card fares at four transit operators in one region: a
metro (MTR), a bus network (BUS), a light rail line (LRT) and a ferry
(FRY). It builds each operator's stations, its cards and the week's taps
at the gates, writes them as CSV, reads them back and parses them with
the back office's checks, closes the week (journeys priced by zones,
peak, transfers and concessions, then capped by day and by week),
writes each operator's journeys and card statements, runs five reports
per operator, and then has the region's reduced-fare fund repay the
operators' concessions in the order they claim.

```
MTR taps: 2,131 rows, 75,679 bytes, fnv 988d1cd6
...
MTR_taps.bad line 51: no station 'MTR-99'
LRT_cards.bad line 21: bad balance '12.5'
FRY_stations.csv line 2: station FRY-01 is not at BUS
...
MTR ridership: 2,131 taps, 1,049 journeys (139 transfers), 26 incomplete, 19 cancelled, 21 stray tap-outs, 40 declined at the gate; by day 183 163 164 194 165 96 84
...
FRY revenue: $4,772.50 at full fare, $618.41 concessions, $692.99 capped off, $3,461.10 charged (70.7% at peak, $161.41 for incomplete journeys); 55 autoloads and 76 top-ups at the machine, $1,860.00 loaded
...
LRT caps: 26 card-days at the day cap, 28 cards at the week cap; most riding 30001402 ADULT 21 journeys $38.10, 30001091 ADULT 21 journeys $32.00, 30001081 ADULT 20 journeys $32.00
...
LRT subsidy: claimed $445.55, repaid $445.55, $0.00 unpaid; $140.49 left in the fund
FRY subsidy: claimed $618.41, repaid $140.49, $477.92 unpaid; $0.00 left in the fund
fund.log: 4 entries, 98 bytes, fnv 84d3be2e
```

## What it does

- **The operators** each have a boarding fare, what each zone crossed
  adds (nothing on the buses, which are flat), a peak fare as a percent
  of the off-peak one, and an adult day cap and week cap.
- **The stations**: 16, 20, 12 and 6, numbered along the line and split
  into two to four fare zones.
- **The cards**: 120 at each operator, adult, senior, student or child,
  with a starting balance and whether the card loads itself.
- **The taps**: each card's rider is a commuter (home to work and back
  on weekdays, sometimes a round trip at the weekend), a regular (one to
  three trips on most days) or an occasional rider. Most trips are a
  tap-in and a tap-out; some change trains halfway (a tap-out and a
  tap-in a few minutes apart), some never tap out, some tap in and
  straight back out, and some tap out twice. The taps are put in time
  order and numbered.
- **The parsers** check the header and final newline, the field count,
  station ids (the operator's, once each), names and zones; card
  numbers (eight digits, the operator's, once each), types, balances
  with two decimals and the autoload flag; and tap numbers in order,
  cards and stations that exist, days, `HH:MM` times that never go
  backwards, and `IN` or `OUT`. They report `file line N: what`. A copy
  of MTR's taps naming a station it does not have, a copy of LRT's cards
  with a balance missing a digit, and FRY's stations read as BUS's show
  three of them.
- **The week's close** takes each tap in order. A tap-in with a journey
  still open charges that journey as incomplete first. A card under the
  boarding fare loads $20 if it loads itself; otherwise it is declined at
  the gate and its rider tops it up by $10 at the machine. A tap-in
  within 45 minutes of the card's last tap-out is a transfer. A tap-out
  at the station tapped in at, within two minutes, cancels the journey
  at no charge; a tap-out with no journey open is a stray. Otherwise the
  fare is the boarding fare plus each zone crossed, raised at the peak
  (weekdays 7:00 to 9:30 and 16:00 to 18:30), less the boarding fare on a
  transfer; the card's concession comes off (half for seniors, 30% for
  students, three quarters for children), and the charge is cut to what
  is left under the day cap and the week cap, both scaled by the same
  concession. A journey never tapped out, found at the card's next
  tap-in, a tap-out on a later day or the end of the week, is charged the
  most the operator charges, less the concession, outside the caps.
- **The journeys and statements** are written per operator and read
  back.
- **The reports**: ridership (taps, journeys, transfers, incomplete,
  cancelled, strays, declines and journeys by day); revenue (full fares,
  concessions, what the caps took off, what was charged, the share at
  peak, the incomplete charges, and the loads); caps (card-days at the
  day cap, cards at the week cap and the three cards ridden most);
  stations (the three busiest by journeys begun, and journeys by zones
  crossed); and balances (what the cards hold, those below zero, the
  lowest, and the average spend by card type).
- **The fund** holds $1,400 for the week. Each operator claims what its
  concessions cost it, and the fund repays what it has left. Every claim
  goes in `fund.log`, which is written and read back.

Everything is integer arithmetic in cents and minutes, so the backends
and the Python mirror `fare_cap.py` agree by construction; the mirror
wrote `expected.out`.

## How the port works

stmt-par: seven groups of statements that do not depend on each other,
in `run()` (`fare_cap.kara` lines):
- L1018-L1021 four station builds and L1022-L1025 four card builds, then
  L1026-L1029 four tap builds over each operator's own stations and
  cards;
- L1032-L1043 twelve `fs.write` calls, each with the encoder that builds
  its text;
- L1060-L1071 twelve reads, each with `?`;
- L1088-L1091 four station parses and L1092-L1095 four card parses, then
  L1096-L1099 four tap parses, which read both. Each can fail with `?`,
  so a parallel run has to report the first failure in source order;
- L1116-L1119 four week closes, the hot group: each prices and caps its
  operator's journeys over data it alone owns;
- L1122-L1129 eight journey and statement writes and L1133-L1140 their
  reads;
- L1151-L1170 twenty reports, five per operator, that only read what the
  earlier groups made.

The four `claim` calls (L1178-L1181) are the case that must stay in
order: each takes `mut fund`, and an operator is repaid only from what
the operators before it left. In order, MTR, BUS and LRT are repaid in
full and FRY gets $140.49 of its $618.41; claimed in reverse, FRY, LRT
and BUS are repaid in full and MTR gets $68.64 of its $546.56.

## Edits from fare_cap.orig.kara to fare_cap.kara

`karac check` raised one error, at `Vec.filled(nc * DAYS as usize, 0)`:
`cannot mix integer types 'i64' and 'usize' in arithmetic`, telling the
writer to cast, with no fix. The cast was the mistake, since `len()` is
an `i64` and `usize` is for FFI only; the edit drops it
(B-2026-10-25-10). Two more sites, `Vec.filled(DAYS as usize, 0)` and
`Vec.filled(op.zones as usize, 0)`, passed the check, though the same
`usize` passed to a function's `i64` parameter is refused
(B-2026-10-25-11); they lost their casts too. After that it matched the
mirror on the first run under the MIR interpreter, the LLVM backend, the
ASan build and the native binary, with nothing on stderr.

## Scope

One week at four operators, each with its own cards, with no fares
shared between operators, no passes or period tickets, no fare
products bought on board, no refunds or balance transfers, no blocked or
replaced cards, no service changes, and nothing after the week.

## Running

```bash
karac run fare_cap.kara
karac build --mir fare_cap.kara && ./fare_cap
python3 -I -B fare_cap.py > expected.out
FARE_REVERSE=1 python3 -I -B fare_cap.py   # the operators claim the subsidy in reverse
```

About 3.5 s on the MIR interpreter (a debug build of `karac`), 2.8 s on
the LLVM backend and under 0.01 s built. It writes its 23 files to the
working directory and removes them at the end.
