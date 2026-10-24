# pipeline/cdr_rating

A mobile operator's monthly rating batch, the job a carrier's billing
system runs when a month closes. For each of four regions (north, coast,
valley and metro) it builds the subscriber book and September's call
detail records, writes them as CSV, reads them back and parses them with
a loader's checks, drops the records a switch delivered twice, rates
every call, text, data session and top-up in time order against the
line's plan, scores every line for fraud, bills the postpaid lines, runs
five reports, and then settles the off-net minutes of all four regions
with the one partner network that terminates them.

```
coast_cdrs.bad line 2078: bad start '2026-09-31 10:00:00'
metro_cdrs.csv line 2: bad record id 'M000001'
...
north classes: most minutes onnet 7027, offnet 4189, fixed 2454; most revenue satellite $1,162.38, premium $659.08, roaming $578.95; 9,835 bundle minutes used
metro postpaid: 387 bills for $20,195.92 (fees $14,094.00, usage $4,562.54, tax $1,539.38), ARPU $52.18; 9 over their credit limit; largest 74100572 $650.17
valley fraud: 2 flagged, 11 to watch; highest 73103731 80 (simbox x160 short); 73105863 80 (simbox x160 short); 73100572 45 (premium roaming)
...
metro interconnect: 840 calls, 3,539 minutes, $21.23 in invoices IC-0013 to IC-0016; rebate CN-0001 of $30.00 at 2026-09-24 17:47:09; month to date 15,827 minutes in tier 3, owed $118.96
settle.log: 17 entries, 609 bytes, fnv cdb5d348
```

## What it does

- A region has 480 lines, each on one of five plans in the region's own
  mix: prepaid (a balance, no bundle), basic, plus, unlimited and
  business. A plan has a monthly fee, a credit limit, a bundle of
  domestic minutes, texts and data, its charging increments (per minute,
  30 s then 6 s, or per second), a data rate beyond the bundle and, for
  business, a discount on international calls. One line in ten was
  activated during the month.
- A region's 5,600 records are calls, texts, data sessions and prepaid
  top-ups from three switches, out of time order. Most traffic is at
  peak hours, a few lines are heavy users, some records are made abroad,
  two lines per region are SIM boxes (160 short calls each to distinct
  off-net numbers), and 80 records are switch retries, the same record
  delivered again later with a new id. Everything comes from a 32-bit
  integer hash of the region's seed.
- The CSV files carry money as dollars with two decimals and times as
  `YYYY-MM-DD HH:MM:SS`. The parsers check the header, the field count,
  the region's letter and digits of every record id, the record type,
  the caller's number against the region, the destination (digits, 4 to
  15 of them, or `-` for data and top-ups), a real time in September
  2026, the amount for the record's type, the roaming country and the
  switch, and report `file line N: what`. A copy of the coast records
  with a start on 31 September, and the metro records read as north,
  show two of the errors.
- Rating takes a region's records in time order, then file order. A
  record before its line's activation is set aside. A call is billed in
  the plan's increments, its class is the tariff's longest matching
  prefix (on-net, off-net, fixed, toll-free, premium, four international
  zones and satellite), and its rate depends on peak hours (Monday to
  Friday, 08:00 to 19:59; the 1st is a Tuesday). Domestic minutes, texts
  and data come out of the bundle first and the rest is charged, rounded
  up to the cent; roaming has its own flat rates. A prepaid line pays
  from its balance, a record it cannot pay for is refused, and a top-up
  adds to the balance for what comes after it.
- Fraud scoring adds points for a SIM box (100 or more distinct off-net
  or fixed numbers in calls under 90 s), a line whose calls are mostly
  short, premium spend over $30, roaming over $50, and a prepaid line
  refused five times. From 50 points a line is flagged; from 30 it is
  watched.
- Billing pro-rates the fee by the days the line was active, adds the
  usage and 8.25% tax (rounded half up), and marks the bills over the
  plan's credit limit.
- The reports: traffic (records, retries dropped, calls and minutes with
  the peak share, texts, data and top-ups); minutes and revenue by class
  and the bundle minutes used; postpaid bills with ARPU, lines over
  their limit and the largest bill; prepaid top-ups, spend, refusals and
  balances; and the fraud flags with the three highest scores.
- The interconnect agreement covers the whole month of off-net minutes
  from all four regions: 1.2 cents a minute up to 6,000 minutes, 0.9 up
  to 12,000 and 0.6 beyond, and a rebate of 0.2 cents on every minute so
  far, credited once, when the month passes 15,000. Each region's
  off-net calls, in time order, take the rate of the tier the month has
  reached minute by minute and are invoiced weekly; invoices and the
  credit note are numbered as they are issued and go in `settle.log`,
  which is written and read back.

Everything is integer arithmetic (cents and tenths of a cent, division
that truncates), so the backends and the Python mirror `cdr_rating.py`
agree by construction; the mirror wrote `expected.out`.

## How the port works

stmt-par: nine groups of statements that do not depend on each other,
in `run()` (`cdr_rating.kara` lines):
- L1167-L1170 four subscriber books, each from its region's seed;
- L1173-L1176 four months of records, each over its own book;
- L1179-L1186 eight `fs.write` calls, each with the encoder that builds
  its text;
- L1197-L1204 eight reads, each with `?`;
- L1212-L1219 eight parses. Each can fail with `?`, so a parallel run has
  to report the first failure in source order;
- L1232-L1235 four dedupes, each over its own `Map` of record keys;
- L1238-L1241 four ratings, the hot group;
- L1244-L1251 four fraud scorings and four bill runs;
- L1254-L1273 twenty reports, five per region, that only read what the
  earlier groups made.

The four `settle` calls (L1282-L1285) are the case that must stay in
order: each takes `mut ic`, a region's minutes land on the minutes of the
regions before it, and invoices and the credit note are numbered as they
are issued. Settled in reverse, metro's calls cost $42.47 in IC-0001 to
IC-0004 instead of $21.23, north's cost $26.22 instead of $50.27, the
rebate goes to north as $30.04 at 2026-09-24 15:41:54, and the month
owes $118.93 instead of $118.96.

## Compiler gaps

None. `karac check` passed the first draft as written, and it matched
the mirror on the first run under the MIR interpreter, the LLVM backend,
the ASan build and the native binary, with nothing on stderr.

## Scope

One month, one currency and one partner network; no number portability
(the prefix decides the class), no multi-party or forwarded calls, no
taxes by destination, no proration of bundles for late activations, and
no partial cut-off of a prepaid call: a record the balance cannot pay
for is refused whole.

## Running

```bash
karac run cdr_rating.kara
karac build --mir cdr_rating.kara && ./cdr_rating
python3 -I -B cdr_rating.py > expected.out
```

About 32 s on the MIR interpreter (a debug build of `karac`), 2.7 s on
the LLVM backend and 0.1 s built. It writes its ten files to the
working directory and removes them at the end.
