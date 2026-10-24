# pipeline/claim_batch

An insurer's claims year, the batch a property and casualty company runs
over its branches. For each of four branches (north, harbor, plains and
metro) it builds the policy book and the year's claims, writes them as
CSV, reads them back and parses them with a loader's checks, scores
every claim for fraud, adjudicates it against its policy, runs five
reports, and then passes the four branches' payments through one shared
stop-loss reinsurance treaty.

```
harbor_claims.bad line 413: bad amount '1250.5O'
metro_policies.csv line 2: bad policy 'MB-P0001'
...
harbor fraud: 4 referred, 44 for review; highest HB-C00859 70 (repeat late large similar x2); HB-C01330 55 (large similar x4); HB-C00033 50 (late new large); likeliest ring V-019 on 34 similar claims
harbor outcomes: 940 paid for $13,754,339.06 (120 at the limit), loss ratio 75.3%; not paid: outside period 126, below deductible 107, late notice 74, not covered 54, limit used 24, no policy 10, referred 1
...
plains ceded: 1020 payments (checks 1941 to 2960), $13,518,541.50 paid, $8,432,717.94 recovered in 14 monthly recoveries RC-0001 to RC-0014; net retained $5,085,823.56; year to date $40,540,900.27, treaty left $567,282.06
treaty.log: 3973 entries, 159,758 bytes, fnv 69b09f69
```

- A branch has 2400 policies (auto, home or travel in the branch's own
  mix), each with a sum insured, a deductible, the insurer's share
  (coinsurance, on home policies), a start date and a premium. Its 1336
  claims name a policy, a peril, the loss and report dates, the amount,
  the repair shop or clinic (the vendor) and the item count. Most losses
  fall inside the policy's year; one storm hits each branch over three
  days; some claims are reported late, some name a policy that does not
  exist or a peril the line does not cover; and one vendor per branch
  files 36 near-identical claims on different policies a few days apart.
  Everything comes from a 32-bit integer hash of the branch's seed.
- The CSV files carry money as dollars with two decimals and dates as
  `YYYY-MM-DD`. The parsers check the header, the field count, the
  branch prefix and digits of every id, the line and peril names, every
  amount, the coinsurance, real calendar dates and a report date not
  before the loss, and report `file line N: what`. A copy of the harbor
  claims with `1250.5O` as one amount, and the metro book read as north,
  show two of the errors.
- Fraud scoring adds points for a late report, a loss in the policy's
  first 30 days, a claim of at least 60% of the sum insured, a round
  amount, a repeat claim on the same policy and peril within 45 days,
  and up to 40 for similar claims (the same vendor and peril on another
  policy, within 10 days and 2% of the amount). The similar-claims scan
  walks every claim's neighbours in loss order, which is where most of
  the time goes. At 50 points a claim is referred; from 35 it is paid
  after a two-week review.
- Adjudication takes the claims in report order: no policy, not covered,
  outside the policy period, notice later than 90 days, referred, below
  the deductible, then the covered amount times the coinsurance, capped
  by what the policy's earlier payments left of its sum insured.
- The reports: claims by peril and the storm's count; referrals, the
  three highest scores with their reasons, and the likeliest ring
  vendor; outcomes with the loss ratio against premium and the reasons
  for not paying; report and payment lags at the median and p90; and
  payments by line.
- The treaty keeps the first $30,000,000 of the four branches' paid
  claims together and pays 80% of what lies above that, up to
  $9,000,000. Each branch's payments, in payment order, take the next
  check number and add to the year's total; the ceded amounts are billed
  to the reinsurer once a month as numbered recoveries. Every check and
  recovery goes in `treaty.log`, which is written and read back.

Everything is integer arithmetic (cents, division that truncates), so
the backends and the Python mirror `claim_batch.py` agree by
construction; the mirror wrote `expected.out`.

stmt-par: seven groups of statements that do not depend on each other,
in `run()` (`claim_batch.kara` lines):
- L1046-L1049 four policy books, each from its branch's seed;
- L1052-L1055 four years of claims, each over its own book;
- L1058-L1065 eight `fs.write` calls, each with the encoder that builds
  its text;
- L1076-L1083 eight reads, each with `?`;
- L1091-L1098 eight parses. Each can fail with `?`, so a parallel run has
  to report the first failure in source order;
- L1111-L1114 four fraud scorings, the hot group;
- L1117-L1120 four adjudications;
- L1123-L1142 twenty reports, five per branch, that only read what the
  earlier groups made.

The four `cede` calls (L1150-L1153) are the case that must stay in
order: each takes `mut tr`, a branch's payments land on what the
branches before it paid, and checks and recoveries are numbered as they
are made. Ceded in reverse, metro's checks start at 1, no recovery
starts until harbor, harbor recovers $6,854,272.17 in RC-0001 to
RC-0011, north gets the last $2,145,727.83, and plains recovers nothing.

## Compiler gaps

None. `karac check` passed the first draft as written, and it matched
the mirror on the first run under the MIR interpreter, the LLVM backend,
the ASan build and the native binary, with nothing on stderr.

## Running

```bash
karac run claim_batch.kara
karac build --mir claim_batch.kara && ./claim_batch
python3 -I -B claim_batch.py > expected.out
```

About 19 s on the MIR interpreter (a debug build of `karac`) and 0.06 s
built. It writes its ten files to the working directory and removes them
at the end.
