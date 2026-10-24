# pipeline/freight_audit

A shipper's freight-payment batch for October 2026, over four carriers:
Parcelo (parcel), Swift (expedited air), Northline (less than truckload)
and Coastal (truckload). It builds the month's bill-of-lading file and
each carrier's invoice file, writes them as CSV, reads them back and
parses them with the audit desk's checks, audits every invoice against
the carrier's contract tariff, runs four reports per carrier, and then
schedules every approved invoice into weekly payment runs that share one
cash budget.

```
northline_invoices.bad line 58: bad amount '520.9'
parcelo_invoices.bad line 301: invoice date 2026-10-06 is before the line above
coastal_invoices.csv line 2: invoice 'C000001' is not swift's
...
parcelo: 939 invoices, $48,659.57 billed, $41,478.82 approved, saved $7,180.75 (14.8%): OK 786, OVER 38, UNDER 12, WGT 37, ACC 31, DUP 26, UNK 9
  OVER over-billed: P000054 B00131 billed $47.37, audited $42.68
...
northline spend: $845,999.97 on 778 shipments, $26.63 a hundredweight; Newark $322,741.13 (271), Columbus $219,141.48 (249), Reno $304,117.36 (258); top lane 071-607 $41,656.29 on 17
coastal service: 123 of 160 on time (76.9%), 3.3 days in transit on average, 25 a day late, 12 two days late; latest late B01970 shipped 10-31, promised 4, took 6
...
coastal paid: 122 invoices, $548,682.65 over 7 runs; 13 took the 2% discount, saving $650.86; 77 paid past net 30, $5,598.55 in late fees; 38 unpaid at the cutoff, $197,831.71
payments: $1,436,614.90 paid, $18,772.15 in discounts taken, $5,598.55 in late fees, 38 invoices unpaid at the cutoff ($197,831.71)
```

## What it does

- **The shipments** are 2,000 bills of lading over the month from three
  distribution centres (Newark, Columbus, Reno) to 60 consignees in 40
  delivery regions, one consignee in five residential. Each goes by one
  carrier: parcels of 1 to 70 pounds with their dimensions, air parcels
  of up to 90, LTL freight of 150 to 8,000 pounds with its freight class
  and liftgate, residential or inside delivery, and truckloads of 9,000
  to 42,000 pounds with their stops and hours waited at the dock. Each
  carries the transit days promised (by distance) and the day it was
  delivered, a day or two late now and then.
- **The invoices** bill each carrier's shipments a day to three after
  delivery. Most bill the tariff exactly; the rest carry the errors an
  audit desk sees: a weight over the BOL's, an accessorial nobody ordered,
  an amount 3 to 12 percent over or 3 to 10 percent under, a difference
  of under a dollar, the same shipment billed again a few days later, and
  bills for shipments the carrier never carried (on no BOL at all, or on
  another carrier's).
- **The parsers** check the header and final newline, the field count,
  the BOL (once each), the dates (shipments in date order, delivery
  within 20 days; invoices in date order), the DC, consignee and lane,
  the carrier's letter and the fields that only its kind of service has,
  the freight class, the accessorial codes (only those the carrier
  offers, in LRI order) and the amount (dollars with two decimals). They
  report `file line N: what`. A copy of Northline's file with one amount
  cut short, a copy of Parcelo's with one invoice dated before the line
  above it, and Coastal's file read as Swift's show three of the errors.
- **The audit** takes each invoice in file order. It rejects a bill for a
  shipment that is not the carrier's (UNK) and a second bill for the same
  shipment (DUP), refunds a Swift delivery that came after its promised
  day (GSR, the service guarantee), and re-rates the shipment at the BOL's
  weight and accessorials by the carrier's tariff: zone (by distance) and
  the larger of actual and dimensional weight for parcel and air, with a
  residential charge and a fuel surcharge; for LTL the hundredweight rate
  by distance and class, the weight break's discount, the contract's 68
  percent off, a minimum, the accessorials and fuel; for truckload the
  larger of a per-mile linehaul and a minimum, extra stops, detention past
  two hours, liftgate and fuel. A bill at the wrong weight (WGT), with an
  unordered accessorial (ACC) or over the tariff by more than a dollar
  (OVER) is approved at the tariff; one under it (UNDER) or within a
  dollar (OK) at what it billed.
- **The reports**: invoices by verdict with the first of each, what was
  billed, approved and saved; spend by DC, per hundredweight and on the
  top lane; on-time delivery against the promise, days in transit, and
  the latest late shipment; and consignees billed, residential
  deliveries and the three with the most spend.
- **The payment schedule** has a run every Friday from 9 October to
  27 November, each with $200,000 to spend. Each approved invoice goes
  into the first run at least three days after its date that has room for
  it, at 2% off within ten days of the invoice and 1.5% more past net 30.
  What no run has room for is unpaid at the cutoff. Every entry goes in
  `payments.log`, which is written and read back.

Everything is integer arithmetic in cents, pounds, inches, miles and
days, so the backends and the Python mirror `freight_audit.py` agree by
construction; the mirror wrote `expected.out`.

## How the port works

stmt-par: seven groups of statements that do not depend on each other,
in `run()` (`freight_audit.kara` lines):
- L1116-L1119 four invoice builds, each over its carrier's seed;
- L1122-L1126 five `fs.write` calls, each with the encoder that builds
  its text;
- L1145-L1149 five reads, each with `?`;
- L1158 the shipment parse, then L1159-L1162 the four invoice parses.
  Each can fail with `?`, so a parallel run has to report the first
  failure in source order;
- L1179-L1182 four audits, the hot group: each builds its own BOL index
  and its own set of BOLs already billed, and re-rates every invoice;
- L1185-L1200 sixteen reports, four per carrier, that only read what the
  earlier groups made.

The four `schedule` calls (L1214-L1217) are the case that must stay in
order: each takes `mut pay`, and the run an invoice lands in, and so
whether it earns the discount, pays the late fee or misses the cutoff,
depends on what the invoices scheduled before it left of each run's
budget. Scheduled in reverse (Coastal first), Coastal pays all 160 of its
invoices at the discount instead of 122 with 77 late, Northline pays 656
with 135 late instead of all 778 at the discount, Swift pays 23 instead
of 135, Parcelo 42 instead of 904, and the batch leaves 1,096 invoices
unpaid ($193,348.68) instead of 38 ($197,831.71).

## Edits from freight_audit.orig.kara to freight_audit.kara

None any more. `ACC_CODES.find(ch)` in `acc_field` and
`CARRIER_CODES.find(f[6])` in `parse_shipments` had to gain a `.clone()`
on the argument, because `karac check` said `find` moved its String
argument when the receiver was a `const` Str (kara ledger row
B-2026-10-24-40). Thread A fixed that in kara be69aae3e9, so the clones
are out and `freight_audit.kara` is the program as first written again.

Both forms matched the mirror on the first run under the MIR
interpreter, the LLVM backend, the ASan build and the native binary, with
nothing on stderr.

## Scope

One month and one shipper, published tariffs fixed for the month (no
fuel index moving week to week), no claims for loss or damage, no
reweigh or reclass certificates (the BOL's weight and class are taken as
right), no disputes sent back to carriers or short-paid invoices, a
service guarantee only on Swift, and runs that never borrow from the next
week's budget.

## Running

```bash
karac run freight_audit.kara
karac build --mir freight_audit.kara && ./freight_audit
python3 -I -B freight_audit.py > expected.out
FREIGHT_REVERSE=1 python3 -I -B freight_audit.py   # the carriers scheduled in reverse
```

About 6 s on the MIR interpreter (a debug build of `karac`), 2 s on the
LLVM backend and 0.03 s built. It writes its eight files to the working
directory and removes them at the end.
