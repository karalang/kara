# pipeline/toll_clearing

A toll clearinghouse's month-end batch for October 2026, over four toll
agencies: the Harbor bridge, the Ridge turnpike, the Lakeshore express
lanes and the Valley tunnel. It builds the prepaid account file, the
stolen-tag hotlist and each agency's transaction file, writes them as CSV,
reads them back and parses them with the clearinghouse's checks, rates
every transaction (who it was, then what it costs), runs four reports per
agency, and then posts every rated trip to one ledger of prepaid balances
that all four agencies draw on.

```
valley_txns.bad line 41: bad tag '2912218853'
ridge_txns.bad line 62: entry at 2026-10-02 05:18:59 is not before the exit
...
harbor: 1,452 transactions, 1,259 rated (1,171 by tag, 88 by plate), 193 not billed: S 12, C 8, D 33, U 55, V 85
  S stolen tag: H0000709 2026-10-16 08:41:01 lane 2 3139166932
...
ridge revenue: $4,788.12 on 1,238 trips; short $474.05 on 442, medium $1,912.85 on 501, long $1,502.40 on 209, no entry $898.82 on 86; plate surcharges $299.52; 45 trips over the account class, $75.40 more
lakeshore traffic: busiest Fri 10-09 with 50, busiest hour 19:00 with 115 over the month, busiest lane 4 with 279; 42.9 a weekday, 13.9 a weekend day
...
harbor posted: 1,259 trips, $7,579.48 from prepaid balances, 196 replenishments for $6,950.00, 94 notices billing $1,270.87; 14 accounts suspended
  suspended after 3 notices: A20218 10-13 19:34, A20041 10-16 10:56, ...
...
month: $19,176.73 debited, $17,065.00 replenished, 602 notices billing $6,246.20; 56 accounts used every replenishment their card allows, 44 suspended
```

## What it does

- **The accounts** are 300 prepaid accounts, each with a transponder tag
  (ten digits, the last a Luhn check digit), a plate from one of four
  states, a vehicle class (2 to 6 axles), an opening balance, a
  replenishment threshold and amount, and the number of times this month
  the card on file may be charged (none for one account in eight). One
  account in 40 closes during the month.
- **The hotlist** has the tags reported stolen, with the time of the
  report.
- **The transactions** come from each agency's panel of accounts (22 to 30
  percent of them): commutes out and back on weekdays, single trips at any
  hour, a tag that is not read on one trip in twelve (a plate read with a
  confidence instead, sometimes too low to use), an axle count one class
  high now and then (a trailer), a second read of the same pass a minute
  later on one trip in thirty, and 70 to 110 vehicles with out-of-state
  plates and no account. On the Ridge every tagged exit carries the entry
  interchange and time; an untagged one does not.
- **The parsers** check the header and final newline, the field count,
  the agency's letter on the transaction id, the time stamp (a time in
  October, never earlier than the line before), the lane, the tag's check
  digit, the plate's shape, the confidence and class, and that an entry
  is only on a Ridge line, has both its fields and comes before the exit;
  the account file also checks for a tag or plate on two accounts, and
  the hotlist for a tag on no account. They report `file line N: what`.
  A copy of Valley's file with one tag's check digit off by one, a copy of
  Ridge's with one entry time after its exit, and Harbor's file read as
  Valley's show three of the errors.
- **Rating** takes each transaction in file order. It identifies the
  vehicle by its tag, or by its plate when no tag was read and the plate
  read is at least 80 percent confident, and rejects it as a stolen tag
  (S, read after the report), a closed account (C), a duplicate read (D,
  the same account within two minutes), an unreadable plate (U) or a plate
  on no account (V). The trip is charged at the larger of the axle count
  and the account's class, by the agency's rule: the bridge's peak hours,
  the turnpike's cents a mile between interchanges (from the farthest end
  when the entry is unknown), the express lanes' price by how many
  vehicles used that direction in that hour (and a flat weekend price),
  and the tunnel's night rate. A trip identified by its plate pays half as
  much again.
- **The reports**: transactions rated and rejected by code with the first
  of each; revenue by fare band, the plate surcharges and the trips
  charged over the account's class; the busiest day, hour and lane and the
  average weekday and weekend day; and the accounts billed, how many paid
  a trip by plate, and the three with the most tolls.
- **The ledger** takes each agency's rated trips in time order. When a
  toll would take the balance below the account's threshold, the card is
  charged the replenishment amount if it has charges left this month; the
  toll is then paid from the balance, or, when the balance cannot cover
  it, billed as a notice (the plate rate plus a $3 fee). The third notice
  suspends the account, and its later trips go straight to notices. Every
  entry goes in `ledger.log`, which is written and read back.

Everything is integer arithmetic in cents and seconds, so the backends and
the Python mirror `toll_clearing.py` agree by construction; the mirror
wrote `expected.out`.

## How the port works

stmt-par: seven groups of statements that do not depend on each other,
in `run()` (`toll_clearing.kara` lines):
- L1179-L1182 four transaction builds, each over its agency's seed;
- L1185-L1190 six `fs.write` calls, each with the encoder that builds
  its text;
- L1212-L1217 six reads, each with `?`;
- L1228-L1229 the account and hotlist parses, then L1230-L1233 the four
  transaction parses. Each can fail with `?`, so a parallel run has to
  report the first failure in source order;
- L1250-L1253 four ratings, the hot group: each keeps its own last-seen
  time per account for the duplicate check, and the express lanes count
  their own congestion;
- L1256-L1271 sixteen reports, four per agency, that only read what the
  earlier groups made.

The four `post_tolls` calls (L1285-L1288) are the case that must stay in
order: each takes `mut led`, and whether a trip is paid from the balance,
after a replenishment, or billed as a notice depends on what the trips
posted before it left of the balance and the card's charges, and 95 of
the 215 accounts with trips use more than one agency. Posted in reverse
(Valley first), Valley bills 57 notices ($540.00) instead of 265
($2,527.86) and suspends 8 accounts instead of 16, Harbor bills 272
($3,746.55) instead of 94 ($1,270.87) and suspends 18 instead of 14, and
the month has 540 notices billing $6,090.43 instead of 602 billing
$6,246.20.

## Edits from toll_clearing.orig.kara to toll_clearing.kara

- `gen` is a reserved word, which the build counter in `build_txns` and
  its field were called. `karac check` says so and `karac fix` writes
  `r#gen`; it is `seq` now, in the mirror too.
- `karac fix` removed the `*` from twelve reads in the two `sort_by`
  closures over a `Vec[i64]`: the item is `Copy`, so the closure gets a
  copy (core-semantics.md §5.10).
- `let mut how = "";` and `let mut band = "no entry";`, both later stored
  in a `Str` field. A string literal bound with no type is a `String`, as
  the design says, but the error lands at the use (`expected 'Str', found
  'String'` at `how: how`, and at the tuple `toll_for` returns) with no
  note pointing at the binding and no fix. That is kara ledger row
  B-2026-10-24-33. `how` is annotated `: Str`.
- With `band` a `Str`, `toll_for`'s result borrows its arguments under the
  signature rule (core-semantics.md §5.4), the express lanes' `counts`
  map among them, which `rate` owns, so `karac check` refused storing the
  band in the outcomes `rate` returns. The rule and the error are right:
  the band is a `String` now.

After those edits it matched the mirror on the first run under the MIR
interpreter, the LLVM backend, the ASan build and the native binary, with
nothing on stderr.

## Scope

One month and one clearinghouse, no interoperable tags from other
agencies' customers, no image review queue (an unreadable plate is simply
unbilled), no disputes or reversals, a duplicate window of two minutes at
one agency, notices that are billed but never paid in the month, and
suspended accounts that are never reinstated.

## Running

```bash
karac run toll_clearing.kara
karac build --mir toll_clearing.kara && ./toll_clearing
python3 -I -B toll_clearing.py > expected.out
TOLL_REVERSE=1 python3 -I -B toll_clearing.py   # the ledger posted in reverse
```

About 10 s on the MIR interpreter (a debug build of `karac`), 2.5 s on the
LLVM backend and 0.03 s built. It writes its nine files to the working
directory and removes them at the end.
