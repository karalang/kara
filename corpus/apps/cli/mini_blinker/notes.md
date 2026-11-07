# mini_blinker

An in-process signal bus in the shape of blinker. Named signals deliver an
event to their receivers in the order they were connected. A receiver is a
plugin (held strongly, or weakly so that the bus does not keep it alive) or
a function: the built-in `metrics` counter, or a temporary tracer that is
disconnected after a number of sends. A plugin may send another signal
while it handles one; a plugin that is already handling a delivery is
skipped rather than entered again. The mailer and audit plugins write
files as they go.

The program writes its own inputs (`data/wiring.conf`, `data/stock.csv`,
`data/events.log`), parses the three files in tasks of their own, wires the
bus, plays the events, and prints a report. `oracle.py` is the reference.
`mk.py` writes the data, and `gen_kara.py` embeds it in the program.

## Common rules

- A **name** (signal, plugin) is 1 to 32 of `a-z 0-9 _`, starting with a
  letter. A **word** (sender, sku, file name) is 1 to 40 of
  `a-z 0-9 - _ .`.
- A **count** is 1 to 9 digits. **Money** is digits with an optional `.`
  and one or two more digits (`12`, `12.5`, `12.50`), held as cents and
  shown with two decimals.
- In `wiring.conf`, `#` starts a comment and words are separated by
  spaces. Blank lines are skipped.

## wiring.conf

```
signal order_placed
plugin inventory inventory low=stock_low
connect mailer order_placed sender=checkout
connect loyalty order_placed weak
connect metrics *
```

Each problem is reported as `wiring.conf:<line>: <message>` and the line is
skipped.

- `signal <name>` (`usage: signal <name>`; `bad signal name '<n>'`;
  `signal <n> already defined`).
- `plugin <name> <kind> [key=value...]` (`usage: plugin <name> <kind>
  [key=value...]`). The name is checked first (`bad plugin name '<n>'`;
  `plugin name 'metrics' is reserved`; `plugin <n> already defined`), then
  the kind (`unknown kind '<k>'`). Each kind takes exactly one setting,
  which must be given (`<kind> needs <key>=<value>`):

  | kind | setting | value |
  |---|---|---|
  | inventory | `low` | a defined signal (`no signal '<v>'`) |
  | mailer | `from` | a word with exactly one `@` |
  | audit | `file` | a word: the file under `data/out/` |
  | fraud | `limit` | money |
  | loyalty | `rate` | a count above 0 |

  A setting is `key=value` (`expected key=value, found '<o>'`), of the
  kind's key (`unknown setting <k> for <kind>`), at most once
  (`duplicate setting <k>`). A bad value is `bad value for <k>: '<v>'`.
- `connect <receiver> <signal|*> [sender=<word>] [weak]` (`usage: connect
  <receiver> <signal|*> [sender=<name>] [weak]` for fewer than three
  words). The receiver is a defined plugin or `metrics` (`no receiver
  '<r>'`), and the signal is defined or `*`, every signal in definition
  order (`no signal '<s>'`). `metrics` cannot be weak (`metrics cannot be
  connected weakly`); any other option is `unknown option '<o>'`. A
  receiver connected to a signal twice is `<r> already connected to <s>`,
  and then none of the line's connections are made. `sender=` restricts
  the connection to sends from that sender.

## stock.csv

The header is `sku,qty,reorder` (else `stock.csv:1: bad header` and no
stock). Each row is checked in this order: `expected 3 fields`,
`bad sku '<s>'`, `duplicate sku <s>`, `bad qty '<q>'`,
`bad reorder level '<r>'`, reported as `stock.csv:<line>: <message>`.

## events.log

Blank and `#` lines are skipped. A line is `<hh:mm:ss> <sender> <signal>
[key=value...]` or `<hh:mm:ss> @ <command> [args]`. A bad time is
`bad time '<t>'`, an event with no signal `missing signal`, a command line
with no command `missing command`, a field without `=` or with an empty key
`bad field '<f>'`.

## Delivery

Every event line is echoed as `> <line>`, and a problem with it as
`  error: events.log:<line>: <message>`. Sending to an undefined signal is
`no signal '<s>'`.

A send at depth `d` prints each receiver's result indented `d + 1` levels
(two spaces each). For each connection, in order:

- A `sender=` connection whose sender differs is skipped silently.
- A muted signal prints `(muted)` once and delivers nothing.
- A function receiver is called. A plugin receiver held weakly is looked up
  by name: alive while the bus holds the plugin or any strong connection
  holds it. A dead one prints `pruned <name>` and the connection is removed
  after the send.
- A plugin already handling a delivery fails with `re-entrant delivery
  skipped`. Otherwise it handles the event and its delivered count goes
  up, whether it succeeded or failed.
- A result prints `<name>: <text>`, a failure `<name> failed: <message>`,
  and no result prints nothing.

A nested send (from a plugin) prints a header `>> <signal> from <sender>:
<fields>` at its own depth, then its results one level deeper.

### Plugins

A missing field in an event is `missing field '<k>'`.

- **inventory** handles `order_placed` and `order_cancelled` (`sku`, `qty`;
  `bad qty '<q>'` unless a count above 0; `unknown sku '<s>'`). A
  cancellation puts the stock back (`released <q> <sku>, <n> left`), and a
  sku back above its reorder level may be flagged again. An order takes
  stock (`out of stock: <sku> has <n>, wants <q>`) and says
  `reserved <q> <sku>, <n> left`. The first time a sku falls to its reorder
  level or below, the plugin sends its `low` signal as sender `inventory`
  with `sku=<sku> left=<n>`, before its own result prints. The plugin holds
  its `low` signal weakly, so the bus and the plugin form no cycle.
- **mailer** mails on `order_placed` and `order_shipped` (`order`,
  `customer`: `order <o> received` or `order <o> shipped`, to the customer)
  and on `stock_low` (`restock <sku>, <left> left`, to `buyer`). Each mail
  is a line `#<n> from <from> to <to>: <subject>` in `data/out/outbox.txt`,
  and the result is `mail #<n> to <to>`.
- **audit** writes `<time> <signal> <sender> <fields>` for every delivery
  to its file and gives no result.
- **fraud** checks `payment_received` (`order`, `amount`; `bad amount
  '<a>'`) and fails with `payment of <a> for order <o> is over the limit
  of <limit>` when the amount is above the limit.
- **loyalty** gives `customer` one point per whole `rate` of `total` on
  `order_placed` (`bad total '<t>'`): `<customer> +<p> points, <n> in all`.
- **metrics** counts the sends delivered to it, per signal.

### Commands

`@ status`, `@ mute <signal>`, `@ unmute <signal>`, `@ drop <plugin>`,
`@ disconnect <receiver> <signal>`, `@ temp <signal> <count>`. Any other
command is `unknown command '<c>'`; a wrong number of arguments is
`usage: @ <cmd> <args>`.

- `status` prints each signal in definition order:
  `<signal>[ [muted]]: <receivers>`, each receiver with ` (weak)` and
  ` (from <sender>)` as they apply, or `none`.
- `mute` / `unmute` print `muted <s>` / `unmuted <s>`.
- `drop` lets the bus stop holding a plugin (`<p> is not held`) and prints
  `dropped <p>`. A plugin no strong connection holds then dies.
- `disconnect` removes the receiver's connection (`<r> is not connected to
  <s>`) and prints `disconnected <r> from <s>`.
- `temp` connects a tracer (`bad count '<n>'` unless 1 to 99), which
  answers `trace #<k>: <fields>`, and prints `trace on <s> for <n> sends`.
  Each top-level send of the signal counts down, and at zero the tracer is
  disconnected and `trace on <s> ended` printed.

## Report

`== load` gives the counts and problems of the three files. `== events`
is the replay above. `== end` prints, for each plugin, `<name>: <n>
delivered; <summary>` or `<name>: gone`, then the live inventory's stock
in file order (`stock <sku>: <n> (reorder at <r>)`), then
`sent <signal>: <n>` from the metrics counter. The plugins' files are then
closed, and `== files` lists every file in `data/out/` in name order with
its line count.

## Compiler gaps found

Four, all fixed in kara before this app landed:

- B-2026-11-07-4: `karac check` refused a move after an iterator chain's
  closure read the same binding when the chain's root held user records,
  when the chain was an `if` condition, or when the move was in another
  `match` arm.
- B-2026-11-07-5: a `let`-bound closure stored in an enum payload
  (`Link.Func(trace)`) captured by reference and was refused with E0509.
- B-2026-11-07-6: a program's private `trait Receiver` broke every
  `karac run` and `karac build` with an error in library source.
- B-2026-11-07-7: a `weak` payload bound by a `match` (and a `Vec[weak T]`
  element bound by a loop) could not be read or upgraded.
