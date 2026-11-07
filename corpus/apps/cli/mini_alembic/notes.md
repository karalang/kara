# mini_alembic

Database schema migrations in the shape of Alembic. Each migration script
names its parent revisions, so the scripts form a graph with branches,
labels and merge points. The tool upgrades and downgrades a schema along
that graph, one transaction per migration. It keeps the current revisions
and the schema in a state file, and prints the SQL an upgrade would run
without running it. It can stamp a version without touching the schema,
check the schema against a replay of its history, and generate new
revision and merge scripts on disk.

The program writes its own inputs (`data/versions/*.mig`,
`data/script.txt`), parses every script in a task of its own, runs the
command script, and prints a report. `oracle.py` is the reference.
`mk.py` writes the data, and `gen_kara.py` embeds it in the program.

## Migration scripts

Scripts are the `*.mig` files under `data/versions` and
`data/out/versions` (the generated ones). Other files are ignored. A
script is shown by its path under `data/` (`versions/1f0a3c_users.mig`),
and scripts are read in path order.

```
# a comment
revision: 1f0a3c
down: none                  # or one parent, or `a, b` for a merge
label: billing              # optional: a name for this revision
message: create users

upgrade:
    create_table users id:int:pk email:text:notnull name:text
downgrade:
    drop_table users
```

Blank lines and `#` lines are skipped. Header lines are `key: value`, and
the first problem stops the script. It is reported as
`<path>:<line>: <message>`, or `<path>: <message>` when it concerns the
whole file, and the script is skipped.

- The key must be lower case, and the `:` must be followed by a space or
  the end of the line (`expected key: value`). The keys are `revision`,
  `down`, `label` and `message` (`unknown key '<k>'`), each at most once
  (`duplicate key '<k>'`).
- A revision id is 6 to 12 of `0-9 a-f` (`bad revision id '<v>'`). `down`
  is `none` or ids separated by `,`. A parent listed twice is
  `down lists <id> twice`.
- A label is 1 to 24 of `a-z 0-9 _`, starting with a letter. It cannot be
  `head`, `heads` or `base` (`bad label '<v>'`). A message cannot be empty.
- `upgrade:` and `downgrade:` start the two sections, each at most once
  (`duplicate section '<s>'`). An operation line is indented and belongs to
  the last section. An operation before both sections is
  `operation outside upgrade/downgrade`. An unindented operation in a
  section is `operation must be indented`. A key in a section is
  `key after the upgrade/downgrade sections`.
- At the end, a missing `revision`, `down`, `message`, `upgrade` or
  `downgrade` is `missing <it>`, checked in that order. A file name that
  does not start with `<revision>_` is
  `file name does not start with revision <id>`.

### Operations

Names are 1 to 30 of `a-z 0-9 _`, not starting with a digit
(`bad name '<n>'`). A column is `name:type` with optional flags `:pk`,
`:notnull` and `:default=<v>`. The types are `int`, `text`, `real`, `bool`
and `date`.

| Operation | Words |
|---|---|
| `create_table <table> <column>...` | at least one column; no column twice |
| `drop_table <table>` | |
| `add_column <table> <column>` | no `pk` |
| `drop_column <table> <column>` | |
| `rename_table <old> <new>` | |
| `rename_column <table> <old> <new>` | |
| `alter_column <table> <column> <change>...` | `type=<t>`, `notnull`, `null` |
| `create_index <index> <table> <c1,c2,...> [unique]` | no column twice |
| `drop_index <index>` | |
| `execute <sql>` | the rest of the line, sent as it is |

A wrong number of words is `usage: <the operation's row>`. The other
errors are `unknown operation '<w>'`, `bad column '<spec>'` (no type),
`unknown type '<t>'`, `unknown flag '<f>'`, `duplicate flag '<f>'`,
`bad default in '<spec>'`, `add_column cannot add a primary key`,
`duplicate column '<c>'`, `unknown change '<c>'`,
`column '<c>' listed twice` and `expected 'unique', found '<w>'`.

## The graph

Once the scripts are parsed, these checks run in order. Each one skips the
scripts it reports:

1. A revision defined by an earlier script:
   `<path>: revision <id> already defined in <first path>`.
2. A label claimed by an earlier script:
   `<path>: label <l> already used by <id>`.
3. A parent that is not among the scripts left:
   `<path>: down revision <id> not found`. This runs again until nothing
   changes, since a skipped script can orphan its children.
4. A revision that does not lead back to one with no parent (a cycle, or
   below one): `<path>: revision <id> does not lead back to a base`.

The skipped scripts are reported in path order, followed by
`loaded <n> revisions from <m> files, <k> skipped`.

The graph order is topological. Of the revisions whose parents are all
placed, the one with the smallest id goes next. A head is a revision with
no children. A branch point has two or more children, and a merge point
has two or more parents.

The applied revisions are always closed under parents. The current
revisions are the applied ones with no applied child, shown in graph
order. They are `base` when none is applied.

### Naming a revision

- `base`: nothing applied.
- `head`: the only head, else
  `multiple heads (<ids>); name one or use heads`.
- `heads`: every head (upgrade, sql, stamp).
- `<label>@head`: the one head at or below the labelled revision, else
  `label <l> has <n> heads (<ids>)`. `no label '<l>'` and
  `bad revision '<spec>'` are the other errors.
- `<label>`: the labelled revision.
- An id, or a prefix of at least 3 characters (`revision prefix '<p>' is
  too short`) matching one id. No match is `no revision '<spec>'`, and
  several matches are `ambiguous revision '<p>' (<ids>)`.
- `+N` (upgrade, sql): N steps from the single current revision, each to
  its only child. From base, a step goes to the only revision with no
  parent. More than one current revision is
  `multiple current heads; name a target`. A step with no child is
  `relative revision +N goes past the head`, and one with several is
  `<id or base> branches to <ids>; name one`.
- `-N` (downgrade): unapplies the single current revision N times. Errors
  are `relative revision -N goes past base` and
  `multiple current heads; name a target`.

## The schema

Tables keep their columns in order, and indexes record their table,
columns and uniqueness. An operation fails with:

- `table <t> already exists`, `no table <t>`, `no column <t>.<c>`,
  `column <t>.<c> already exists`, and `index <i> already exists` or
  `no index <i>`.
- `table <t> still has index <i>` when dropping a table with an index.
- `column <t>.<c> is not null and has no default` for an added column.
- `cannot drop primary key column <t>.<c>`,
  `column <t>.<c> is used by index <i>` and
  `cannot drop the last column of <t>` when dropping a column.
- `a primary key column cannot be null`.

Renaming a table or a column carries its indexes along. `execute` changes
nothing.

A migration is one transaction. If a step fails, the schema is put back as
it was before that migration, the command stops, and it prints
`failed <id> at step <k> (<operation>): <error>; rolled back`. The
migrations before it stay applied.

## Commands

`data/script.txt` holds one command per line, and `#` lines are comments.
Each command is echoed as `> <line>`, and an error is printed as
`  error: <message>`. After every command the state is written to
`data/out/state.txt`: a `version <id>` line per current revision, then
`table`, `column` and `index` lines.

- `current`, `heads`: the current revisions (a head marked `(head)`), and
  the heads with their labels.
- `history`: every revision, latest first, as
  `<parents or base> -> <id>` followed by its tags (label, `(head)`,
  `(branchpoint)`, `(mergepoint)` and `(current)`) and `, <message>`.
- `upgrade <target>`: applies every unapplied ancestor of the target, the
  target included, in graph order, and prints
  `upgrade <parents or base> -> <id>, <message>` for each. If there are
  none, it prints `nothing to upgrade`. Upgrading to `base` is an error.
- `downgrade <target>`: unapplies every applied revision below the target,
  in reverse graph order, running each one's downgrade section, and prints
  `downgrade <id> -> <parents or base>, <message>`. The target must be
  applied (`revision <id> is not applied`), and `base` unapplies
  everything.
- `sql <target>`: the SQL an upgrade would run, between `BEGIN;` and
  `COMMIT;`, without changing anything. From base it first creates the
  `alembic_version` table. Each migration gets a `-- upgrade ...` comment,
  then its statements and the version bookkeeping. With no current parent
  that is an `INSERT`. Otherwise it is an `UPDATE` of the first current
  parent and a `DELETE` of each other one. A failing step prints
  `-- failed ...` and `ROLLBACK;`.
- `stamp <target>`: sets the applied revisions to the target and its
  ancestors, leaving the schema alone, and prints `stamped <current>`.
- `check`: replays the applied revisions' upgrades on an empty schema. It
  prints `schema matches`, or `schema differs` followed by the replayed
  lines missing from the schema and the extra lines, or
  `replay fails at ...`.
- `schema`: the tables and indexes in name order.
- `show <target>`: one revision's file, label, message, parents, children
  and steps.
- `revision <label|-> <parent|base> <message...>`: writes an empty
  revision script to `data/out/versions/<id>_<slug>.mig`, reads it back
  and adds it to the graph. The id is the FNV-1a 32-bit hash of
  `<message>|<parents joined by ,>` in 8 hex digits. The slug is the
  message in lower case, with each run of other characters turned into one
  `_`, cut to 24 characters, with no `_` at either end.
- `merge <message...>`: the same, with every head as a parent. It needs
  two heads (`nothing to merge: one head`).
- `restart`: saves the state, reloads the scripts (printing only the
  `loaded` line) and the state file, and prints
  `restarted at <current>, <n> tables, <m> indexes`.

Any other word is `unknown command '<w>'`. A command that takes a target
with none, or with more than one, is `<cmd> needs a target` or
`<cmd> takes one target`. At the end the program prints
`ran <n> commands`.

## Compiler gaps found

None. The first draft passed `karac check` after `karac fix`, which
replaced `vec![..]` with list literals, dropped two `*` on a `bool`
pattern binding, and made a `match` on a reused `Option` bind by `ref`.
It also refused Rust's `trim_matches`, naming `strip_prefix` and
`strip_suffix`. The first run matched the oracle on all four backends.
Probes of other natural forms also behaved: a `mut ref` schema assigned
through `*sc = work`, a hand-written `Display`, `sort_by_key` over a
`Map` index, in-place edits of struct-variant payloads, and a
`mut ref` binding in an `if let`.
