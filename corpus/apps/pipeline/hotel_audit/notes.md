# pipeline/hotel_audit

A hotel group's night audit at the end of a week, the job each property
runs after midnight to close the day, here for the seven nights from
Saturday 17 to Friday 23 October 2026. For each of four hotels (Albany,
Bristol, Carmel and Denver) it builds the reservation book and the week's
folio postings, writes them as CSV, reads them back and parses them with
the property system's checks, fills the room-night grid, prices every
night, charges the fees, opens the folios, and runs five reports. Then it
posts every checked-out stay of a loyalty member to the group's one
loyalty ledger.

```
albany_reservations.bad line 301: bad arrival '2026-10-32'
carmel_postings.csv line 2: bad reservation 'C100096'
...
carmel occupancy: 91.4% (576 of 630 room-nights); peak Mon 10-19 100.0%; ADR $288.78, RevPAR $264.03; 24 walked for $2,880.00, 0 oversold
denver tax: city tax 15.75% $30,505.91, occupancy fee $2,794.00, remit $33,299.91; best night Fri 10-23 $35,347.29
albany folios: 393 checked out, $193,264.18 due, $1,451.66 in credit; 177 still in house; largest A100273 Petrov Z. $2,842.56
...
M0013 to gold at carmel with 62,765 points
tiers: 195 member, 65 silver, 5 gold, 0 platinum; 2,285,178 points issued
loyalty.log: 564 entries, 21,372 bytes, fnv 16f9a259
```

## What it does

- A hotel has king, queen and suite rooms (180, 120, 90 and 240 rooms in
  all), a rack rate for each type, a city tax in basis points of room
  revenue and a flat occupancy fee per room-night. Its book holds 3.7
  reservations per room: a guest, a loyalty number for two in five, a room
  type, a rate code (BAR, AAA, CORP, GRP or OTA), an arrival from three
  days before the week to its last night, one to seven nights, a booking
  date and a status: booked, cancelled (with the day it was cancelled) or
  no-show. Everything comes from a 32-bit integer hash of the hotel's
  seed.
- The postings are what the outlets send to the folios: up to four
  incidental charges a stay (room service, minibar, parking, spa,
  restaurant) on nights of the stay, a deposit for one stay in three, and
  one charge in ninety keyed to a mistyped reservation number.
- The parsers check the header and the final newline, the field count,
  the hotel's letter and digits of every reservation number, the guest,
  the loyalty number, the room type, the rate code, dates (real days of
  2026), the nights, the status, a booking date before the arrival, a
  cancellation date between the two and present exactly when the status
  is cancelled, the department and an amount with two decimals, and
  report `file line N: what`. A copy of the Albany book with an arrival on
  32 October, and the Carmel postings read as Denver's, show two of the
  errors.
- The audit goes night by night and room type by room type. When more
  booked guests are in house than there are rooms, it walks that night's
  arrivals that booked last, at $120 each for a night at the partner
  hotel, and a walked reservation leaves the book. A guest who arrived
  before the night and cannot be walked would count as oversold. Each
  room-night is priced at the rate code's share of the rack rate (BAR,
  AAA and OTA 15% higher on Friday and Saturday), and OTA bookings owe
  the agency 18% commission.
- A no-show pays one night, and so does a cancellation less than two days
  before arrival; both are taxed with the room revenue. A folio is the
  whole stay's room revenue with its tax and fees, plus the incidentals,
  less the deposits; a posting with no folio (a mistyped reservation, or
  a guest who was walked) goes to suspense.
- The reports: occupancy (sold room-nights, the peak night, ADR, RevPAR,
  walks); revenue by segment with the fees, the commission and the net;
  the tax remittance and the best night; incidentals by department and
  suspense; and folios, those checked out with what is due and in credit
  and the largest balance, and those still in house.
- The loyalty ledger opens each member at a balance from before the week
  and posts the hotel's checked-out member stays in check-out order: 10
  points a dollar of room revenue, times the multiplier of the tier the
  member holds as the stay posts (member, silver at 20,000 points, gold at
  45,000, platinum at 90,000; 1x, 1.25x, 1.5x, 2x). A member who passes a
  threshold moves up at once, and reaching gold pays a 2,500-point bonus.
  Every posting goes in `loyalty.log`, which is written and read back.

Everything is integer arithmetic (cents and points, division that
truncates), so the backends and the Python mirror `hotel_audit.py` agree
by construction; the mirror wrote `expected.out`.

## How the port works

stmt-par: seven groups of statements that do not depend on each other,
in `run()` (`hotel_audit.kara` lines):
- L961-L964 four reservation books, each from its hotel's seed;
- L967-L970 four posting logs, each over its own book, each sorted;
- L973-L980 eight `fs.write` calls, each with the encoder that builds
  its text;
- L991-L998 eight reads, each with `?`;
- L1006-L1013 eight parses. Each can fail with `?`, so a parallel run has
  to report the first failure in source order;
- L1026-L1029 four audits, the hot group: each fills its own grid, walks
  its own guests, and opens its own `Map` of folios;
- L1032-L1051 twenty reports, five per hotel, that only read what the
  earlier groups made.

The four `post_stays` calls (L1059-L1062) are the case that must stay in
order: each takes `mut lg`, a stay earns at the tier its member holds
when it posts, and a member's earlier stays at other hotels decide that
tier. Posted in reverse (Denver first), Denver's stays earn 576,323
points instead of 618,346 and Albany's 628,334 instead of 590,250, M0067,
M0077 and M0024 reach gold at Albany instead of at Carmel and Denver,
M0047 stays silver, and four members end gold instead of five, with
2,271,362 points issued instead of 2,285,178.

## Compiler gaps

None. `karac check` passed the first draft as written, and it matched
the mirror on the first run under the MIR interpreter, the LLVM backend,
the ASan build and the native binary, with nothing on stderr.

## Scope

One week, one currency and one rate per stay night; no room moves or
upgrades, no packages, no split folios or city ledger, no house use or
complimentary rooms, no re-rating of rates already posted in an earlier
audit, and a walk that is simply the latest booking rather than a choice
by rate or loyalty tier.

## Running

```bash
karac run hotel_audit.kara
karac build --mir hotel_audit.kara && ./hotel_audit
python3 -I -B hotel_audit.py > expected.out
HOTEL_REVERSE=1 python3 -I -B hotel_audit.py   # the ledger posted in reverse
```

About 7 s on the MIR interpreter (a debug build of `karac`), 2.5 s on
the LLVM backend and 0.02 s built. It writes its ten files to the
working directory and removes them at the end.
