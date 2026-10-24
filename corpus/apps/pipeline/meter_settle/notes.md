# pipeline/meter_settle

A weekly smart-meter run, the job an electricity retailer does once the
meter data for the week has arrived. For each of four regions (north,
coast, valley and metro) it builds a week of half-hourly reads for every
meter, writes them as CSV, reads them back and parses them with a
loader's checks, estimates the reads that never arrived, bills every
meter on its time-of-use tariff and prices two alternatives for the
residential ones, builds the region's load profile, runs five reports,
and then settles the four regions' demand against one wholesale
portfolio.

```
coast.bad line 301: bad read at 13:00 '4.5'
metro.csv line 2: bad meter 'ME-00001'
...
north bills: $9,471.52 in all; average res $27.69, com $146.98, ind $458.45; largest NR-00017 $720.66, NR-00014 $698.81, NR-00071 $624.39
...
valley peak: 486 kW at Fri 17:30, low 239 kW at Sun 01:00, load factor 78.3%; at the peak res 13.1%, com 10.0%, ind 76.8%
...
valley settled: 64,020 kWh (base 24.5%, mid 58.3%, peaker 17.0%), $5,163.31 at $80.65/MWh, 3 shortfall tickets from SF-0001; margin on energy $5,733.05
metro settled: 39,948 kWh (base 0.3%, mid 27.7%, peaker 34.0%), $16,559.50 at $414.52/MWh, 5 shortfall tickets from SF-0004; margin on energy -$8,667.10
settlement.log: 36 entries, 2,077 bytes, fnv 410a60cc
```

- A region has 96 meters, residential, commercial or industrial in the
  region's own mix, and some residential meters have rooftop solar. A
  meter's 336 half hours are its base load times its kind's daily shape
  (morning and evening peaks at home, office hours at work, flat shifts
  with weekend cuts at a plant) times up to 15% noise; homes also follow
  the day's temperature, and solar subtracts a midday curve scaled by
  the day's cloud, so a solar meter's net read can go negative (an
  export). About one read in 173 never arrives. Everything comes from a
  32-bit integer hash of the region's seed and the meter.
- The CSV has one row per meter and day: the meter id, kind, solar flag,
  day number and 48 reads, with an empty field for a missing read. The
  parser checks the header, the field count, the id's region prefix and
  digits, the kind, the flag, that each meter's days come in order, and
  every read, and refuses an export on a meter without solar; it reports
  `file line N: what`. A copy of the coast file with `4.5` in one read,
  and the metro file read as north, show two of the errors.
- Estimation fills each run of missing reads by straight-line
  interpolation between the reads either side, or with the nearest read
  at either end of the week.
- Billing splits each meter's imports into peak (16:00 to 21:00 on
  weekdays), shoulder and off-peak, and adds the export credit, the daily
  supply charge, a demand charge on the week's highest half hour for
  commercial and industrial meters, and 10% GST. A residential meter is
  also priced on a flat rate and on a demand tariff, and the report
  counts who would pay less on each.
- The reports: imports, exports and estimated reads; the bills by kind
  and the three largest; the split across the three bands; the region's
  peak and lowest half hour, its load factor and each kind's share at
  the peak; and the tariff comparison.
- The wholesale portfolio offers a fixed amount of energy in every half
  hour from three tranches (base, mid, peaker) at rising prices. A region
  buys its net import for each half hour from what is left, cheapest
  first; beyond the peaker it pays the scarcity price, and each run of
  short half hours raises one numbered shortfall ticket. Each day's
  purchases and every ticket go in `settlement.log`, which is written and
  read back.

Everything is integer arithmetic (Wh, hundredths of a cent, division
that truncates toward zero), so the backends and the Python mirror
`meter_settle.py` agree by construction; the mirror wrote `expected.out`.

stmt-par: eight groups of statements that do not depend on each other,
in `run()` (`meter_settle.orig.kara` lines, the same in
`meter_settle.kara`):
- L720-L723 four regions of meters, each from its region's seed;
- L726-L729 four `fs.write` calls, each with the `encode_meters` call
  that builds its text;
- L740-L743 four reads, each with `?`;
- L750-L753 four parses. Each can fail with `?`, so a parallel run has to
  report the first failure in source order;
- L766-L769 four estimates;
- L773-L776 four billing runs, three tariffs over 336 half hours for each
  of 96 meters, which is where most of the time goes;
- L779-L782 four load profiles;
- L785-L804 twenty reports, five per region, that only read what the
  earlier groups made.

The four `settle` calls (L817-L820) are the case that must stay in
order: each takes `mut mkt`, a region gets only the energy the regions
before it left, and shortfall tickets are numbered as they are raised.
Settled in reverse, metro buys everything from the base tranche at
$44.99/MWh, valley has no shortfall, coast raises 12 tickets from
SF-0001, north pays $428.76/MWh and loses money on energy, and the log
grows to 45 entries.

## Edits from meter_settle.orig.kara to meter_settle.kara

Both kinds were my mistakes, and `karac check` named each:

- Three report lines wrote `{parts.join(\", \")}`, escaping the quotes
  inside the f-string's braces. Inside an interpolation a string uses
  plain quotes, `{parts.join(", ")}`, and the lexer's message says so,
  but it reaches the user wrapped as `Expected expression, found
  Error("…")` with its quotes escaped again, and `karac fix` has no edit
  for it. Filed as kara ledger row B-2026-10-24-27.
- The four `settle(mkt, ...)` calls pass a local to a `mut ref Market`
  parameter, which takes a `mut` marker at the call. `karac fix` added
  all four.

After those edits it matched the mirror on the first run under the MIR
interpreter, the LLVM backend, the ASan build and the native binary, with
nothing on stderr.

## Running

```bash
karac run meter_settle.kara
karac build --mir meter_settle.kara && ./meter_settle
python3 -I -B meter_settle.py > expected.out
```

About 14 s on the MIR interpreter (a debug build of `karac`) and 0.03 s
built. It writes its six files to the working directory and removes them
at the end.
