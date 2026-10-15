# cli/csv_query

A query tool for CSV files, a small SQL engine of the kind behind `q`,
csvsql and DuckDB's CSV reader. It writes four CSV files, loads them back
(quoted fields, doubled quotes, embedded commas, empty cells as NULL, an int
or text type inferred per column, a ragged row and an unterminated quote
reported with their line), and runs 30 queries:

    SELECT [DISTINCT] expr [AS name], ... | *
    FROM table [alias] [[LEFT] JOIN table [alias] ON expr] ...
    [WHERE expr] [GROUP BY column, ...] [HAVING expr]
    [ORDER BY expr|alias|ordinal [ASC|DESC], ...] [LIMIT n [OFFSET m]]

Expressions have SQL's three-valued NULL logic, integer arithmetic,
comparisons, AND / OR / NOT, IS [NOT] NULL, [NOT] LIKE with `%` and `_`,
[NOT] IN, UPPER / LOWER / LENGTH, and COUNT(*), COUNT, SUM, MIN, MAX and
AVG (two places, in integers). Grouping is strict: a bare column in a
grouped query must be in GROUP BY. Errors name the clause (unknown and
ambiguous columns, aggregates in WHERE, division by zero, a bad ordinal)
and, for syntax, the offset. Results print as aligned tables.

The mirror `csv_query.py` is the same loader, parser and evaluator written
by hand in Python (no `csv` or `sqlite3` module) and wrote `expected.out`.
Its results for the 17 queries that succeed were also checked against
sqlite3 on the same tables: they agree (sorted where the query has no
ORDER BY, AVG compared at two places).

Written 2026-10-15 by the real-world apps thread; karac at 0e5d2694d3.

## Edits from csv_query.orig.kara to csv_query.kara

Author errors (the diagnostics were right, or the spec says so):
- `const KEYWORDS: [Str; 22]` is Rust; Kāra spells it `Array[Str, 22]`.
- `"(),.*".contains(c)` with a `char`: `Str.contains` takes a `Str`
  (docs/library/strings.md), so `c.to_string()`.
- `Expr == Expr.Star` needs `#[derive(PartialEq)]` on the `shared enum`.
- `struct Parser` shadows the prelude's `Parser` (a warning); renamed
  `SqlParser`.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-15-7 (new): check refuses `match self.toks[self.at] {
  Tok.Sym(t) => t == s, .. }` as a move out of an index, though `==` only
  borrows, and refuses the explicit `ref t` too. The parser binds the
  token first: `let tok = ref self.toks[self.at]; match tok { .. }`.
- B-2026-10-15-8 (new): a user `struct Unit` with a heap field makes the
  MIR interpreter fail on `fs.write`'s `Result[Unit, IoError]` ("ADT place
  holds Unit"), with no shadowing warning. The accumulator is `Pending`.
- B-2026-10-14-52 (widened): iterating a `const` array of `Str` and
  comparing the elements aborts both compiled surfaces in
  `karac_mir_bytes_eq`. `is_keyword` walks a local copy.
- B-2026-10-14-60: returning a `ref` found by a `for` over `db.iter()` is
  refused; `find_table` uses an index loop.
- B-2026-10-14-53 (widened): `fs.write(path, ..)` reports `path` moved;
  `path.clone()`.
- B-2026-10-14-68 (widened): in `vec![("customers", CUSTOMERS), ("unclosed",
  "x,y..")]` the literals are `String` and the const is `Str`, so the
  tuples do not unify; the second text is a const too.

Went in clean: a recursive `shared enum` AST, precedence-climbing
recursive descent over `mut ref self`, `map_err(..)?` chains, tuple
returns destructured in `match`, `sort_by` with a closure over the query,
`Option` fields matched with `Some(ref w)`.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-14-52 is fixed, so
`is_keyword` walks `KEYWORDS` itself rather than a local copy. Each of
these is the original line again, and the app still matches on all four
MIR surfaces.
