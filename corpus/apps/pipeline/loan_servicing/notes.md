# pipeline/loan_servicing

A lender's servicing month for October 2026, over four books of loans:
mortgage, auto, student and personal. It builds each book's loan tape and
the month's payment file, writes them as CSV, reads them back and parses
them with the servicing desk's checks, posts every payment through the
servicing waterfall, runs four reports per book, and then queues every
book's delinquent accounts at one collections desk that has a fixed
number of call slots a day.

```
auto_payments.bad line 91: bad amount '894.2'
student_payments.bad line 251: date 2026-10-05 is before the line above
personal_loans.csv line 2: loan 'P00001' is not mortgage's
...
mortgage cash: 380 payments, $2,075,483.82 (ACH 184 $482,529.49, CHK 63 $174,401.32, CARD 65 $150,564.02, WIRE 68 $1,267,988.99); 3 returned, $8,067.01; 4 unidentified, $4,612.50; 6 refunds, $7,782.66
  applied to fees $6,296.26, interest $225,463.85, principal $461,948.08, escrow $168,860.19, curtailments $1,089,549.20; 76 loans hold $133,924.93 in suspense
mortgage roll: 11 cured, 88 rolled worse, 295 stayed, 6 paid off
  current: 316 -> current 277, 30 34, paid off 5
  30: 40 -> current 6, 30 5, 60 29
...
mortgage book: 394 loans, $63,606,874.77, 5.09% weighted rate; 6 paid off; top states AZ $6,651,225.66 (44), TX $6,625,264.87 (40), IL $6,507,219.38 (37)
mortgage actions: 17 referred to foreclosure ($2,037,197.74); 74 late fees, $5,510.09; 3 NSF fees; first M00045, 120 days past due, $45,615.40
...
personal desk: 153 accounts, $1,344,635.80: 0 worked, 67 part-worked, 86 not reached ($734,122.25)
desk: 800 of 800 calls booked; 11-02 80, 11-03 80, ...
```

## What it does

- **The loan tapes** hold 400 mortgages, 700 auto loans, 500 student
  loans and 600 personal loans, each with its state, months on the book
  and term (30 or 15 years, 3 to 6 years, 10 years, 2 to 5 years), its
  rate, the balance left after its age, the scheduled payment (the
  month's interest plus the balance spread over the months left), escrow
  for taxes and insurance on a mortgage, the due day, the installments
  already past due (most loans none, a few up to four or five, a student
  loan up to nine), the late fees owed, funds held in suspense, and whether it is
  on autopay.
- **The payment files** carry the month's payments in date order: autopay
  drafts on the due day; checks, cards, ACH transfers and wires a few
  days early or up to three weeks late; some paying extra principal, some
  paying the loan off by wire, some splitting the payment in two, some
  paying half or nothing; delinquent borrowers paying nothing, one
  installment, everything they owe, or half. Some checks come back unpaid
  (NSF) a few days later, and a few payments name loans the book does not
  hold.
- **The parsers** check the header and final newline, the field count,
  the loan number (the book's letter, once each), the state, the term
  (one the book writes), the age, the rate (in the book's range), every
  amount (dollars with two decimals), escrow only on a mortgage, the due
  day and the autopay flag; and for payments the number (P for a payment,
  R for a return, once each), the date (in October, in order), the loan,
  the channel, and that a return names a check above it on the same loan
  for the same amount. They report `file line N: what`. A copy of auto's
  payments with one amount cut short, a copy of student's with one
  payment dated before the line above it, and the personal tape read as
  the mortgage book's show three of the errors.
- **The posting** takes each payment in file order. A returned check is
  not applied and its loan owes the $25 NSF fee; a payment for a loan the
  book does not hold is unidentified cash; one for a loan already paid
  off is refunded. A payment made after the 15-day grace period, while
  the month's installment is still unpaid, first assesses the late fee
  (5% of the scheduled payment, up to the book's cap). Then the money goes, with
  any suspense, to fees, then to whole installments oldest first (the
  interest on the balance, the rest of the payment to principal, and
  escrow), then, once nothing is past due, to principal as a curtailment
  or, when it covers the balance, to a payoff with the excess refunded.
  What is less than an installment while one is still due waits in
  suspense. At month end the late fee falls on a loan whose installment
  is still unpaid after a grace period that ends in October, and a loan four installments
  behind (nine for a student loan, five for a personal loan) goes to the
  book's action: foreclosure, repossession, default or charge-off.
- **The reports**: cash by channel with the returns, unidentified cash
  and refunds, and where it was applied; the roll from each delinquency
  bucket to where it ended; the book at month end (a charged-off loan
  leaves it), its weighted rate and its three largest states; and the
  actions, late fees and NSF fees.
- **The collections desk** has 80 call slots on each business day from
  2 to 13 November. Each book's delinquent accounts that are not in an
  action, the most installments behind and then the largest balance
  first, get a call for each installment behind, up to three, on
  separate days, each in the first day after the last one with a slot
  left. Every call goes in `collections.log`, which is written and read
  back.

Everything is integer arithmetic in cents and basis points, so the
backends and the Python mirror `loan_servicing.py` agree by
construction; the mirror wrote `expected.out`.

## How the port works

stmt-par: seven groups of statements that do not depend on each other,
in `run()` (`loan_servicing.kara` lines):
- L1048-L1051 four tape builds and L1052-L1055 four payment builds, each
  over its book's seed;
- L1058-L1065 eight `fs.write` calls, each with the encoder that builds
  its text;
- L1084-L1091 eight reads, each with `?`;
- L1103-L1110 four tape parses, then four payment parses. Each can fail
  with `?`, so a parallel run has to report the first failure in source
  order;
- L1127-L1130 four postings, the hot group: each builds its own loan
  index and set of returned checks and runs every payment through the
  waterfall;
- L1133-L1148 sixteen reports, four per book, that only read what the
  earlier groups made.

The four `queue_book` calls (L1162-L1165) are the case that must stay in
order: each takes `mut desk`, and which accounts get their calls depends
on how many slots the books queued before them took. Queued in reverse
(personal first), personal and student are worked in full, auto has 26
accounts not reached and 9 part-worked, and none of the 89 mortgage
accounts is reached, instead of mortgage, auto and student in full and
86 personal accounts not reached.

## Edits from loan_servicing.orig.kara to loan_servicing.kara

`karac check` raised four errors and `karac fix` applied all four: three
bindings of a Vec element (`let ln = loans[i];` twice and
`let orig = out[at];`) became `ref loans[i]` and `ref out[at]`, since the element is not
`Copy`, and `id_ok(f[2], book.code, 5)` passes the `Str` field to a
`ref String` parameter, so the fix adds `.to_string()`. No compiler gaps:
after the fix it matched the mirror on the first run under the MIR
interpreter, the LLVM backend, the ASan build and the native binary, with
nothing on stderr.

## Scope

One month, interest charged a month at a time on the balance when an
installment is paid (no daily accrual or interest on arrears), escrow
collected but never disbursed, no partial-payment rules beyond suspense,
no forbearance, deferment or modification, no payoff quotes good through
a date, returned checks found before posting rather than reversed after,
and a desk that takes every call it books.

## Running

```bash
karac run loan_servicing.kara
karac build --mir loan_servicing.kara && ./loan_servicing
python3 -I -B loan_servicing.py > expected.out
LOAN_REVERSE=1 python3 -I -B loan_servicing.py   # the books queued in reverse
```

About 5 s on the MIR interpreter (a debug build of `karac`), 2 s on the
LLVM backend and 0.02 s built. It writes its eleven files to the working
directory and removes them at the end.
