# mini_transitions

State machines in the shape of pytransitions. A machine has variables,
nested states (`on.armed.away`), named conditions over its variables,
named actions, and transitions guarded by `if=` / `unless=` with
`before=` / `after=` actions. States run callbacks as they are entered and
left. A transition leaves states up to the common ancestor of where it is
and where it goes, and enters down to the target. An action may trigger an
event on any machine: a plain machine handles it at once, nested; a queued
machine that is already busy runs it after the current one.

The program writes its own inputs (`data/machines.conf`, `data/events.log`),
parses the two files concurrently, plays the events, and prints a
report. `oracle.py` is the reference. `mk.py` writes the data, and
`gen_kara.py` embeds it in the program.

## Common rules

- A **name** (machine, var, cond, action, trigger) is 1 to 32 of
  `a-z 0-9 _`, starting with a letter. A **state** is names joined by `.`;
  `a.b` is a child of `a`.
- A **number** is an optional `-` and 1 to 9 digits.
- In both files `#` starts a comment and words are separated by spaces.
  Blank lines are skipped.

## machines.conf

```
machine door
var keys 1
state closed initial
state locked
cond has_key keys > 0
action note_lock set locked_at 1
transition lock closed -> locked if=has_key after=note_lock
on_enter locked note_lock
```

`machine <name>` starts a machine (`usage: machine <name>`;
`bad machine name '<n>'`; `machine <n> already defined`), and every later
line adds to it until the next `machine` line. A line before any machine,
or after a bad `machine` line, is `no machine to add to`. Each problem is
reported as `machines.conf:<line>: <message>` and the line is skipped. A
machine with no initial state is dropped once its lines end
(`machines.conf: machine <n> has no initial state`).

- `queued` (`usage: queued`): the machine queues events that arrive while
  it is busy.
- `var <name> <number>` (`usage: var <name> <value>`; `bad var name '<n>'`;
  `var <n> already defined`; `bad value '<v>'`).
- `state <path> [initial] [final]` (`usage: state <path> [initial]
  [final]`; `bad state name '<p>'`; `state <p> already defined`; a child's
  parent must exist, `no state '<parent>'`, and not be final, `final state
  <parent> cannot have children`; `unknown flag '<f>'`; `initial state
  already set to <s>`).
- `cond <name> <var> <op> <number>` with `op` one of `== != < <= > >=`
  (`usage: cond <name> <var> <op> <value>`; `bad cond name '<n>'`;
  `cond <n> already defined`; `no var '<v>'`; `bad operator '<o>'`;
  `bad value '<v>'`).
- `action <name> <op> ...` (`usage: action <name> <op> ...` for fewer than
  three words; `bad action name '<n>'`; `action <n> already defined`):
  - `say <words>`: prints `<machine> says: <words>`
    (`usage: action <name> say <words>`).
  - `add <var> <number>` / `set <var> <number>` (`usage: action <name>
    add <var> <value>`, likewise `set`; `no var '<v>'`; `bad value '<v>'`).
  - `trigger [<machine>] <event>`: sends the event to that machine, or to
    its own (`usage: action <name> trigger [<machine>] <event>`). The target
    is looked up when the action runs.
  - anything else is `unknown action '<op>'`.
- `transition <trigger> <src> -> <dst> [options]` (`usage: transition
  <trigger> <src> -> <dst> [options]`). The trigger is a name
  (`bad trigger name '<t>'`) not starting with `to_` (`trigger names
  starting with to_ are reserved`). `src` is a state or `*` (any state but
  a final one), and not final (`final state <s> has no exits`); `dst` is a
  state or `=` (stay: an internal transition). An undefined state is
  `no state '<s>'`. Options are `if=<cond>`, `unless=<cond>`,
  `before=<action>`, `after=<action>` (`unknown option '<o>'`;
  `duplicate option <k>`; `no cond '<c>'`; `no action '<a>'`).
- `on_enter <state> <action>` / `on_exit <state> <action>` (`usage:
  on_enter <state> <action>`; `no state '<s>'`; `no action '<a>'`). A
  state may have several, run in the order given.
- Any other first word is `unknown directive '<d>'`.

## events.log

Blank and `#` lines are skipped. A line is `<machine> <trigger>`
(`usage: <machine> <trigger>`; `no machine '<m>'`) or `@ <command>
[args]` (`missing command`).

## Running a trigger

Every event line is echoed as `> <line>`, and a problem with it as
`  error: events.log:<line>: <message>`. A trigger runs at depth `d`
(0 for an event line) and prints its lines indented `d + 1` levels (two
spaces each). An action's `trigger` runs one level deeper and prints
`-> <machine> <event>` first (`no machine '<m>'` if there is none). A
trigger deeper than 5 prints `<machine>: too deep, <event> dropped`.

A queued machine that is busy appends the event to its queue and prints
`queued <machine> <event>`. Otherwise it is busy while it runs the event
and then its queue in order, each as `run queued <machine> <event>` at the
same depth; after three queued events the rest are dropped with
`<machine>: queue limit, dropped <n>`.

To run trigger `t` in state `s`:

- `to_<state>` goes straight to that state (`<machine>: no state '<x>'`,
  refused), unless `s` is final (refused, as below).
- Otherwise the candidates are the transitions on `t` from `s`, then from
  each of its ancestors in turn, then from `*` unless `s` is final. With
  none, `<machine>: can't <t> from <s>` and the trigger is refused. The
  first candidate whose `if=` holds and whose `unless=` does not fires.
  When every one is held back, `<machine>: <t> blocked in <s>`.

Firing a transition counts it, then runs `before=`. An internal one prints
`<machine>: <s> -> <s> (internal)`. Otherwise, with `k` the number of
leading ancestors the two paths share (one fewer when the target is the
source or one of its ancestors, so that it is left and entered again), it
prints `exit <state>` for each state left, innermost first, each followed
by its `on_exit` actions; then `<machine>: <s> -> <dst>`; then
`enter <state>` for each state entered, outermost first, each followed by
its `on_enter` actions. Last it runs `after=`.

### Commands

`@ state <machine>`, `@ set <machine> <var> <number>`,
`@ triggers <machine>`. Any other command is `unknown command '<c>'`; a
wrong number of arguments is `usage: @ <cmd> <args>`; an unknown machine is
`no machine '<m>'`.

- `state` prints `<machine>: <state> (<var>=<value> ...)`, without the
  parentheses for a machine with no variables.
- `set` changes a variable (`no var '<v>'`; `bad value '<v>'`) and prints
  `set <machine>.<var> = <value>`.
- `triggers` prints `<machine> in <state>: <triggers>`: the triggers of
  the transitions from the state, its ancestors and (unless it is final)
  `*`, sorted and without repeats, or `none`.

## Report

`== load` gives the machines (`machines.conf: <n> machines`), the problems
in file order, then each machine as `<name>: <n> states, <n> transitions,
starts in <state>[, queued]`, then `events.log: <n> lines`. `== events` is
the replay above. `== end` prints each machine as `<name>: <state> after
<n> transitions, <n> blocked, <n> refused[; <var>=<value> ...]`.

## Compiler gaps found

Two, both fixed in kara before this app landed:

- B-2026-11-07-12: a part of a `shared` value held in a local
  (`match m.initial { Some(i) => .. }` with `m` a handle) was refused as a
  move out of a `shared` value, by `karac check` and every backend, though
  core semantics §4.6 makes the binding a `ref`.
- B-2026-11-07-13: a closure that matched a captured enum against dotted
  unit variants (`Cmp.Lt => x < v`) typed as `OnceFn` on the MIR pipeline,
  so storing it in a `Fn` slot failed every run and build while `karac
  check` passed.
