# pipeline/flight_rebook

An airline's day of irregular operations at four hubs: thunderstorms at
ORD in the afternoon, snow at DEN in the morning, an air traffic control
program at ATL from 11:00 to 20:00, and wind at JFK in the evening. It
builds each hub's schedule, its passenger records and the operations
center's events, writes them as CSV, reads them back and parses them
with the airline's checks, rebooks every disrupted passenger onto the
seats that are left, writes the gate agents' rebooking files, runs four
reports per hub, and then hands the operations center's four spare
aircraft to the hubs that ask first.

```
ORD flights: 70 rows, 3,159 bytes, fnv 8754567b
ORD pnrs: 3,997 rows, 130,474 bytes, fnv da37cab8
...
ORD_pnrs.bad line 301: bad party '5'
ATL_events.bad line 6: bad event 'DLA'
JFK_flights.csv line 2: flight NS4000 does not touch DEN
...
ORD ops: 70 flights, 10 cancelled (WX 10), 18 delayed, 58 min average delay, worst NS1013 BOS-ORD 13:05 +135
...
ORD disrupted: 667 PNRs, 1,147 passengers: 356 on cancelled flights, 284 with a cancelled inbound, 27 missed connections
  passengers: 82 same day (0 kept their outbound, 63 downgraded), 436 next day, 629 stranded
  PLT: 139 passengers, 28% same day, 69% next day, 2% stranded
...
ORD care: meals $11,100.00 (471 vouchers), hotels $15,707.00 (113 rooms), credits $148,474.46, refunds $58,834.00, downgrade refunds $12,137.98, bonus miles 2,015,000
...
ORD spares: N801NS to PHX at 07:00 tomorrow, 109 of 109 stranded passengers, $13,325.79 in refunds saved
ORD spares: N802NS to BNA at 07:00 tomorrow, 102 of 102 stranded passengers, $8,964.60 in refunds saved
...
JFK spares: none left for STL (128 stranded), AUS (59 stranded)
spares.log: 261 entries, 6,264 bytes, fnv e99490f4
```

## What it does

- **The schedules**: each hub flies three or four round trips a day to
  ten of 24 cities, out between 06:00 and 21:00 and back so that each
  lands by 23:00, on five aircraft types (E175 to A321) picked by the
  length of the trip, each with its business and economy seats and the
  seats still open on tomorrow's same flight.
- **The bookings** fill each flight to a load of its own (60 to 100% in
  business, 78 to 98% in economy): about 45% of each outbound flight's
  seats go to parties connecting from an inbound flight that lands one to
  four hours before it (never from the city they are flying to), the rest
  to local parties, and the inbound seats left to parties ending at the
  hub. A party is one to four people with a record locator, a cabin, a
  flexible or basic fare, an elite tier (none, silver, gold, platinum)
  and the party's total fare.
- **The events**: what leaves or lands in the hub's weather window is
  cancelled at the hub's rate or delayed by its range; outside it there
  are maintenance cancellations, crews that do not reach an outstation,
  and short delays for traffic, maintenance or crew.
- **The parsers** check the header and final newline, the field count,
  flight ids (once each), routes (one end at the hub), times, aircraft
  and seats; record locators (the airline's 32-letter alphabet, once
  each), the party, cabin, fare (no basic fare in business), tier,
  amount, that the inbound flight lands at the hub and the outbound
  leaves it, that a connection is legal (45 minutes, not back where it
  started), and that no flight is oversold; and for events that the
  flight exists, the file is in flight order, and that a cancellation has
  no minutes and a delay some. They report `file line N: what`. A copy
  of ORD's bookings with a party of five, a copy of ATL's events with a
  misspelled event and JFK's schedule read as DEN's show three of them.
- **The rebooking** applies the events, finds every record whose
  outbound flight is cancelled, whose inbound flight is cancelled, or
  whose delayed inbound now lands less than 45 minutes before its
  delayed outbound, and rebooks them highest tier first, then business,
  flexible fares, and booking order. Each party stays together on the
  earliest arrival that works: in its cabin today, in economy today, in
  its cabin tomorrow, in economy tomorrow, or it is stranded. A segment
  that still works is kept (a connecting party whose inbound was
  cancelled keeps its outbound if the new inbound makes it), and a seat
  given up goes back for the records after it.
- **The rebooking files** list every disrupted record in the order it
  was rebooked, with the cause, the outcome, the new flights (`+1` for
  tomorrow), the cabin and the arrival.
- **The reports**: operations (cancellations by reason, delays, the
  worst); outcomes by cause and by tier, which shows the priority order
  at work; loads on the flights that flew and tomorrow's open seats
  taken; and the care owed: meal vouchers for a wait of two hours or
  five, a hotel room for two for a night at the hub, travel credits for
  arriving three or six hours late or the next day, refunds of flexible
  fares and credits for basic ones when stranded, 40% back for a
  downgrade, and bonus miles for gold and platinum members.
- **The spares**: each hub asks for extra sections tomorrow at 07:00 for
  its two destinations with the most stranded passengers, if at least
  25. A spare seats 150, boarding the stranded parties in priority order.
  Every boarding goes in `spares.log`, which is written and read back.

Everything is integer arithmetic in minutes and cents, so the backends
and the Python mirror `flight_rebook.py` agree by construction; the
mirror wrote `expected.out`.

## How the port works

stmt-par: eight groups of statements that do not depend on each other,
in `run()` (`flight_rebook.kara` lines):
- L1387-L1390 four schedule builds, then L1391-L1394 four booking builds
  and L1395-L1398 four event builds, each over its own hub's schedule;
- L1401-L1412 twelve `fs.write` calls, each with the encoder that builds
  its text;
- L1429-L1440 twelve reads, each with `?`;
- L1457-L1460 four schedule parses, then L1461-L1468 four booking parses
  and four event parses, which read their hub's parsed schedule. Each can
  fail with `?`, so a parallel run has to report the first failure in
  source order;
- L1485-L1488 four rebookings, the hot group: each applies its hub's
  events, classifies every record and searches every disrupted party's
  new flights over seats it alone owns;
- L1491-L1494 four rebooking-file writes and L1498-L1501 their reads;
- L1508-L1523 sixteen reports, four per hub, that only read what the
  earlier groups made.

The four `ask_spares` calls (L1538-L1547) are the case that must stay in
order: each takes `mut pool`, and a hub gets spares only if the hubs
before it left some. In order, ORD and DEN take all four and ATL and JFK
get none; asked in reverse, JFK and ATL take them and DEN's 213
stranded for SFO and ORD's 109 for PHX get none.

## Edits from flight_rebook.orig.kara to flight_rebook.kara

`karac check` raised three errors and `karac fix` applied all three.
`LOC_ALPHA.contains(c)` passes a `char` where `contains` takes a `Str`,
so the fix writes `c.to_string()`; and two `search` calls pass the hub's
`Str` code to a `ref String` parameter, so the fix adds `.to_string()`.
No compiler gaps: after the fix it matched the mirror on the first run
under the MIR interpreter, the LLVM backend, the ASan build and the
native binary, with nothing on stderr.

## Scope

One day, with tomorrow's schedule the same as today's and only its open
seats to sell; no other airlines, no standby lists, no splitting a party,
no rebooking through another hub, no crew legality, no aircraft swaps
beyond the spares, and passengers who take every rebooking they are
given.

## Running

```bash
karac run flight_rebook.kara
karac build --mir flight_rebook.kara && ./flight_rebook
python3 -I -B flight_rebook.py > expected.out
FLIGHT_REVERSE=1 python3 -I -B flight_rebook.py   # the hubs ask for spares in reverse
```

About 11 s on the MIR interpreter (a debug build of `karac`), 3 s on the
LLVM backend and 0.05 s built. It writes its 19 files to the working directory and removes them at
the end.
