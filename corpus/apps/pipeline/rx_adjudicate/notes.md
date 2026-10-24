# pipeline/rx_adjudicate

A pharmacy benefit manager's month-end claims batch for October 2026,
over four pharmacies (Ashford, Brookline, Cedar and Dalton). It builds the
formulary, the member roster and each pharmacy's prescription claims,
writes them as CSV, reads them back and parses them with the claim
system's checks, adjudicates every claim (the edits that reject it, then
the price), runs four reports per pharmacy, and then posts every paid
claim to the members' accumulators, which split it between the member and
the plan.

```
ashford_claims.bad line 41: bad DEA number 'AM3942719'
brookline_claims.csv line 2: bad rx 'B2020001' for cedar
...
ashford claims: 327 received, 240 paid, 87 rejected (26.6%); 65 7, 70 27, 75 8, 76 13, 79 21, 88 11
  65 patient not covered: A1010119 M00093 atorzole 10-15
...
brookline pricing: allowed $61,683.48 (ingredient $66,246.84, fees $218.75); awp 9, mac 116, u&c 46; generic rate 67.3%; DAW 2 penalties $594.55 on 1 fill
ashford controlled: 9 CII and 26 CIV fills paid, 1,995 units; top prescriber AM3942718 with 6; 1 DUR override; busiest day Mon 10-26 with 13 paid
ashford posted: 240 paid claims, member share $16,341.61 plus $1,503.29 in penalties, plan paid $106,711.51; 12 deductibles met, 3 out-of-pocket maximums reached
M00201 met the $1,500.00 deductible at ashford on 10-01
...
month: members paid $76,394.47, the plan paid $488,665.52; most paid M00046 $4,229.05, M00199 $3,841.98, M00047 $3,628.64
```

## What it does

- **The formulary** has 48 drugs, each with an 11-digit NDC, a tier
  (generic, preferred brand, non-preferred brand, specialty), an AWP per
  unit, a MAC for generics, a generic equivalent with its own AWP and MAC
  for one brand in three, a DEA schedule (II or IV for one drug in ten
  each), a therapeutic class and a covered flag.
- **The roster** has 240 members on three plans (bronze, silver, gold:
  deductibles of $1,500, $500 and none; tier copays; specialty
  coinsurance of 30, 25 and 20%; out-of-pocket maximums of $6,000, $4,000
  and $2,500), each with the year's deductible and out-of-pocket spending
  before October, a termination date for one member in 25, and the
  specialty drugs they hold a prior authorization for.
- **The claims** come from each pharmacy's prescriptions: a member of the
  pharmacy's panel, a drug, a days supply and quantity, a first fill on a
  day of October and refills as the supply runs out, one refill in twelve
  brought in early; schedule II drugs are never refilled. Each claim
  carries the prescriber's DEA number, a dispense-as-written code and,
  for one in six, a DUR override code. Ashford and Dalton draw on the same
  members, Brookline and Cedar on others.
- **The parsers** check the header and final newline, the field count,
  the pharmacy's letter on the rx number, that the member and the NDC
  exist, that the fill date is a day of October 2026, the quantity, days
  supply and refill number, the DEA number's two letters and check digit
  (the first, third and fifth digits plus twice the second, fourth and
  sixth), the DAW code and the DUR code, and report `file line N: what`.
  A copy of Ashford's claims with one check digit off by one, and
  Brookline's claims read as Cedar's, show two of the errors.
- **Adjudication** takes each claim in file order and rejects it with
  the first edit that fails: 65 coverage ended, 70 drug not covered, 76
  more than 90 days (30 for schedule II) or more than four units a day,
  88 a refill of a schedule II drug, 75 a specialty drug without prior
  authorization, 79 refill too soon (less than three quarters of the last
  paid fill's supply used), and 88 an interaction with a still-active
  paid fill of an interacting class, unless the claim carries a DUR
  override. A paid claim is priced on the dispensed product: the AWP less
  the pharmacy's discount or the MAC, whichever is lower, plus the
  dispensing fee, unless the pharmacy's usual and customary price is
  lower still. A brand with a generic is substituted under DAW 0, priced
  as the brand under DAW 1, and under DAW 2 (the patient asked for the
  brand) priced as the generic with the difference charged to the member
  as a penalty.
- **The reports**: claims received, paid and rejected by code with the
  first claim of each code; the allowed amount, ingredient cost and fees
  with how many claims each price basis decided, the generic dispensing
  rate and the DAW 2 penalties; the top three drugs by allowed amount; and
  the controlled-substance fills with the prescriber who wrote the most,
  the DUR overrides and the busiest day.
- **The accumulators** take each pharmacy's paid claims in fill order.
  The member pays toward the deductible until it is met, then the tier's
  copay or the specialty coinsurance, never more than what is left of
  the out-of-pocket maximum; the plan pays the rest, and a DAW 2 penalty
  is paid on top and counts toward neither. For the members who filled at
  more than one pharmacy, the deductibles met and maximums reached are
  listed, and every posting goes in `accumulators.log`, which is written
  and read back.

Everything is integer arithmetic in cents, so the backends and the
Python mirror `rx_adjudicate.py` agree by construction; the mirror wrote
`expected.out`.

## How the port works

stmt-par: seven groups of statements that do not depend on each other,
in `run()` (`rx_adjudicate.kara` lines):
- L1182-L1185 four claim builds, each over its pharmacy's seed;
- L1188-L1193 six `fs.write` calls, each with the encoder that builds
  its text;
- L1207-L1212 six reads, each with `?`;
- L1223-L1224 the formulary and roster parses, then L1225-L1228 the four
  claim parses that check against them. Each can fail with `?`, so a
  parallel run has to report the first failure in source order;
- L1241-L1244 four adjudications, the hot group: each keeps its own fill
  history for the refill and interaction edits;
- L1247-L1266 twenty reports, five per pharmacy, that only read what the
  earlier groups made.

The four `post_claims` calls (L1280-L1283) are the case that must stay
in order: each takes `mut acc`, and what a member pays for a claim
depends on how much of the deductible and out-of-pocket maximum the
claims posted before it used. Posted in reverse (Dalton first), Dalton's
members pay $21,526.35 instead of $18,814.40 and meet 15 deductibles
instead of 9, Ashford's pay $13,209.39 instead of $16,341.61 and meet 6
instead of 12, and the month's member share is $75,974.20 instead of
$76,394.47.

## Compiler gaps

None. `karac check` passed the first draft as written, and it matched
the mirror on the first run under the MIR interpreter, the LLVM backend,
the ASan build and the native binary, with nothing on stderr.

## Scope

One month, one claim per fill with no reversals or rebills, no
compounds, no coordination of benefits, no mail order, no step therapy
or quantity-limit overrides, prior authorizations that never expire,
and a refill-too-soon and interaction history that only covers the
claims of the same pharmacy in this batch.

## Running

```bash
karac run rx_adjudicate.kara
karac build --mir rx_adjudicate.kara && ./rx_adjudicate
python3 -I -B rx_adjudicate.py > expected.out
RX_REVERSE=1 python3 -I -B rx_adjudicate.py   # the accumulators posted in reverse
```

About 3 s on the MIR interpreter (a debug build of `karac`), 2 s on the
LLVM backend and 0.02 s built. It writes its eight files to the working
directory and removes them at the end.
