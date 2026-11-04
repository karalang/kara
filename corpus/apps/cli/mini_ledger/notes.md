# mini_ledger

Plain-text double-entry accounting, in the shape of ledger-cli, hledger
and Beancount: journals of dated directives, transactions that must
balance, stock lots held at cost and sold first in, first out, currency
conversions, balance assertions, a bank CSV imported through rules, and
the reports people run at the end of a month. Each input file is parsed
in a task of its own.

```
> lots Assets:Broker
Assets:Broker:Stocks AAPL
  2025-05-12  1 AAPL {201.25 USD}  201.25 USD
Assets:Broker:Stocks MSFT
  2025-08-04  5 MSFT {410.00 USD}  2,050.00 USD
  2026-02-09  6 MSFT {398.60 USD}  2,391.60 USD
```

## Journals

`data/*.journal` hold one directive per line; `;` starts a comment
anywhere on a line, and a line starting with `#` is a comment. Dates are
`YYYY-MM-DD` and must exist.

- `DATE open ACCOUNT [COMMODITY,...]` opens an account, optionally
  limited to those commodities. `DATE close ACCOUNT` closes it at the end
  of that day.
- `DATE balance ACCOUNT AMOUNT` asserts that ACCOUNT and its sub-accounts
  hold exactly AMOUNT of that commodity at the start of the day.
- `DATE price COMMODITY AMOUNT` gives a market price.
- `DATE FLAG PAYEE [| NARRATION]`, with FLAG `*` or `!`, starts a
  transaction; its postings follow on indented lines, up to a blank or
  unindented line. A posting is `ACCOUNT [AMOUNT [COST] [PRICE]]`.

An account name is segments joined by `:`, at least two, the first one of
`Assets`, `Liabilities`, `Equity`, `Income` and `Expenses`, the rest
starting with a capital letter or digit and holding letters, digits and
`-`. An amount is a number (an optional `-`, digits with optional `,`
grouping, an optional fraction of up to six digits) and a commodity (a
capital letter, then one to nine capitals or digits).

## Weights and balancing

Each posting has a weight, and a transaction's weights must sum to zero
in every commodity.

- `10 AAPL` weighs 10 AAPL.
- `100.00 EUR @ 1.0843 USD` weighs the quantity times the price, and
  `@@ 108.43 USD` weighs the total (with the quantity's sign). Either one
  also counts as a price of EUR in USD on that day.
- `10 AAPL {150.00 USD}` adds a lot to the account, held at that cost per
  unit, and weighs the quantity times the cost. `-5 AAPL {}` takes units
  out of the account's AAPL lots, first in first out, and weighs minus
  their cost; `-5 AAPL {150.00 USD}` takes them only from lots at that
  cost. A price after a cost is only a price: it does not change the
  weight. Taking out more units than the lots hold refuses the
  transaction.

One posting may leave out its amount; it takes the negative of what is
left, in each commodity left over, one posting per commodity in name
order. Products are rounded half away from zero to six decimals.

Each commodity has a display precision, the most decimals written in a
posting's quantity or a balance amount anywhere in the input (2 when it
appears only in prices and costs). A transaction balances when every
commodity's sum rounds to zero at that precision, a filled-in amount is
rounded to it, and a balance assertion holds when the account's total
rounds to the asserted amount.

## Checking

The directives are processed in date order; within a day, opens first,
then balance assertions, then transactions and prices, then closes, and
otherwise in file name and line order. A transaction is refused, with its
file, line and the reason, when it posts to an account that was never
opened, is not open yet or is closed, posts a commodity the account does
not take, leaves out more than one amount, has nothing left to fill in,
does not balance, or sells more than it holds. A refused transaction
changes nothing. A failed balance assertion is reported with what the
account held.

## The bank import

`data/bank.csv` is a card statement as a bank exports it: a header row
naming `Date`, `Description`, `Amount` and `Balance`, dates as
`MM/DD/YYYY`, newest row first, quoted fields where a description or an
amount holds a comma, and amounts that may carry a `$`. `data/bank.rules`
names the card's `account` and `currency`, a `default` account, and
`match PATTERN => ACCOUNT [=> PAYEE]` rules, the first whose pattern
appears in the description (ignoring case) winning. Each row becomes a
transaction in date order: flag `*` when a rule matched and `!` when the
default took it, the rule's payee or else the description, the card
posting with the row's amount and the other account's posting left out.
The newest row's balance becomes a balance assertion on the next day.

## Writing it back

Every accepted directive is written to `data/out/ledger.journal` in
processing order, transactions with the left-out amounts filled in and a
blank line after each. The program reads that file back, checks it again,
and compares every account's balance with the first pass.

## Reports

`data/reports.txt` holds one command a line; dates are inclusive.

- `balance [--at DATE] [--depth N] [ACCOUNT...]`: each account's total
  with its sub-accounts, as a tree under the named accounts (all of them
  when none is named), amounts right-aligned in 20 columns, then a rule
  and the total of the tree's roots. `--depth N` hides accounts deeper
  than N segments.
- `register ACCOUNT [--from DATE] [--to DATE]`: each posting to the
  account or under it, with a running total in the posting's commodity.
- `value COMMODITY [--at DATE]`: every asset and liability converted at
  the latest price on or before the date, or the latest inverse price when
  there is no direct one; a holding with neither is listed but not
  totalled.
- `income FROM TO`: the Income and Expenses trees over the period, then
  the net income.
- `lots ACCOUNT`: the open lots under the account, oldest first.
- `print FROM TO`: the written-back directives over the period.

The last commands are an empty tree, an unknown account, a missing
commodity, a period that ends before it starts, a bad depth and an
unknown command.

## Running it

The program writes its own `data/` inputs first (embedded at the end of
`mini_ledger.kara` by `gen_kara.py`):

```
karac run mini_ledger.kara
karac build mini_ledger.kara && ./mini_ledger
```

Both print `expected.out`, and `data/out/ledger.journal` matches the
oracle's byte for byte.

## The oracle

`oracle.py` is the same ledger written plainly in Python from the rules
above. To regenerate:

```
python3 -s -P -B mk.py
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py
```

## Gaps found

Two false ownership errors in `karac check`, both fixed in the compiler
(the ids are in kara's ledger, `docs/bugs-v2.jsonl`, under source
`example:mini_ledger`):

- A task spawned in each arm of an `if`, each taking the same file's
  path and text, was refused as moving them twice, though no run takes
  both arms; the loop around them did not help, though the two values are
  declared afresh on each turn.
- `match p.cost { Cost.At(ca) => .. }` on a posting from `.iter()` was
  taken as moving the field, and so clashed with a closure that reads
  `p.cost`; a field of a borrowed place is matched in place.
