# Data: the `kara-data` package

**Status: post-v1 package (M4c).** `kara-data` is library code, not `std` and not the prelude ([deferred.md § M4c data](../deferred.md#m4c-data)). This page is the API it ships: `Column[T]`, `DataFrame`, the expression type `Expr`, `LazyFrame`, `Stats`, CSV and Arrow IPC. `Tensor` and the numerical sub-libraries (`linalg`, `fft`, `autograd` and the rest) get their own page. A program lists `kara-data` under `[dependencies]` in `kara.toml`; the package declares the import name `data`, so its items are imported from `data` ([design.md § Packages and manifests](../design.md#packages-and-manifests)). **Not yet implemented:** nothing on this page runs on the v2 pipeline yet.

Design calls that were open when this page was written are marked **Decision Qn** where they arise and collected in [Decisions](#decisions).

## What it is for

The package is the table-shaped half of a general-purpose program: the batch job that cleans an export and loads it somewhere else, the report that rolls a month of orders up by region, the script that reads an access log and finds the slow routes. It is not a warehouse engine. A query over a handful of joins and a few million rows belongs here, and a 20-way join over a terabyte belongs in DuckDB, which reads the same Arrow data ([Arrow IPC](#arrow-ipc)).

```kara
import data.{Column, DataFrame, DataError, col};
import std.regex.{Regex};

// A line of the access log:  2026-10-28T19:02:11Z GET /api/orders 503 182
fn load(text: Str) -> Result[DataFrame, DataError] {
    let re = Regex.compile("^(\\S+) (\\S+) (\\S+) (\\d+) (\\d+)$").unwrap();
    let mut route: Column[String] = Column.new();
    let mut status: Column[i64] = Column.new();
    let mut ms: Column[i64] = Column.new();
    for line in text.lines() {
        match re.captures(line) {
            Some(c) => {
                route.push(c.get(3).unwrap().text);
                status.push(i64.parse(c.get(4).unwrap().text).unwrap());
                ms.push(i64.parse(c.get(5).unwrap().text).unwrap());
            }
            None => {}
        }
    }
    let mut df = DataFrame.new();
    df.insert("route", route)?;
    df.insert("status", status)?;
    df.insert("ms", ms)?;
    Ok(df)
}

fn slow_routes(df: DataFrame) -> Result[DataFrame, DataError] {
    let errors = df.filter(col("status").ge(500))?;
    let by_route = errors
        .group_by(["route"])
        .agg([
            col("ms").count().alias("errors"),
            col("ms").quantile(0.95).alias("p95_ms"),
        ])?;
    by_route.sort_by(["errors"], descending: true)
}
```

## Types

| Type | What it is |
|---|---|
| `Column[T]` | One nullable 1-D column with the Arrow layout: a values buffer and a validity bitmap. `T` is a [column element type](#element-types) |
| `DataFrame` | A table: named columns of one length, in schema order. Not generic, since its columns have different element types |
| `Schema`, `Field`, `DataType` | The names and types of a frame's columns, read at run time |
| `Expr` | A column expression (`col("ms").mean()`), the argument of `filter`, `agg`, `with_columns` and `sort_by` |
| `GroupBy` | A frame grouped by key columns, waiting for `agg` |
| `LazyFrame`, `LazyGroupBy` | A query plan over a frame, run by `collect` |
| `Stats` | Statistics over a dense `Slice[f64]`, in the shape of NumPy |
| `DataError` | Why a data operation failed |

## Element types

A column holds one Arrow primitive or string type. The trait `ColumnType` marks them and cannot be implemented outside the package:

| Kāra type | Arrow type |
|---|---|
| `bool` | Boolean |
| `i8`, `i16`, `i32`, `i64` | Int8 … Int64 |
| `u8`, `u16`, `u32`, `u64` | UInt8 … UInt64 |
| `f32`, `f64` | Float32, Float64 |
| `String` | Utf8 |

Arithmetic needs `T: Numeric`, the integer and float primitives ([deferred.md](../deferred.md#m4c-data)). `Column[String]` and `Column[bool]` have no arithmetic.

**Decision Q1.** Dates and timestamps (Arrow Date32, Timestamp) are the next element types any ETL or log job asks for. The first release leaves them out and reads such cells as `String`.

## Missing values: nulls and NaN

A slot is either valid or **null**, recorded in the validity bitmap. Null follows SQL three-valued logic: `null + x` is null, `null == x` is null, never `false`. NaN in a `Column[f64]` is a valid float and follows IEEE 754; it is never turned into a null implicitly. Both rules, and why they stay separate, are in [deferred.md § Column](../deferred.md#column--nullable-1d-data). `fillna(v; treat_nan_as_null: true)` is the one call that treats them alike.

Every reduction skips nulls: `mean` of `[1, null, 3]` is `2.0`, over two values.

## `Column[T]`

### Construction and access

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new() -> Column[T]` | Empty. `T` comes from the binding's annotation or a later `push`, as for `Vec.new()` |
| `with_capacity` | `fn with_capacity(cap: i64) -> Column[T]` | Empty, with room for `cap` slots |
| `from_vec` | `fn from_vec(values: own Vec[T]) -> Column[T]` | Every slot valid |
| `from_options` | `fn from_options(values: own Vec[Option[T]]) -> Column[T]` | `None` is a null slot. The baked surface's name for it is `from_iter_nullable`, though it takes a `Vec` (Decision Q2) |
| `push` | `fn push(mut ref self, value: own T)` | Appends a valid slot |
| `push_null` | `fn push_null(mut ref self)` | Appends a null slot |
| `len` | `fn len(self) -> i64` | Slots, null ones included |
| `null_count` | `fn null_count(self) -> i64` | Null slots. A NaN is not counted |
| `valid_count` | `fn valid_count(self) -> i64` | `len() - null_count()` |
| `is_null` | `fn is_null(self, i: i64) -> bool` | Panics if `i` is out of range |
| `get` | `fn get(self, i: i64) -> Option[ref T]` | The value at `i`, `None` for a null slot. Panics if `i` is out of range. `c[i]` is not a form: `Index.index` returns `ref Output` and cannot answer `None` for a null slot (Decision Q3) |
| `iter` | `fn iter(self) -> impl Iterator[Item = Option[ref T]]` | Every slot in order |
| `valid` | `fn valid(self) -> impl Iterator[Item = ref T]` | The valid values in order, nulls skipped. The baked surface calls it `iter_valid` and returns a `Vec` |
| `to_vec` | `fn to_vec(self) -> Vec[Option[T]]` | Needs `T: Clone` |
| `slice` | `fn slice(self, start: i64, len: i64) -> Column[T]` | `len` slots from `start`. Panics if the range is out of bounds |

### Null handling and transforms

| Method | Signature | Notes |
|---|---|---|
| `fillna` | `fn fillna(self, value: T; treat_nan_as_null: bool = false) -> Column[T]` | Null slots replaced by `value`. With `treat_nan_as_null`, NaN slots too; the flag has no effect on a non-float column |
| `dropna` | `fn dropna(self) -> Column[T]` | Only the valid slots, in order |
| `is_null_mask` | `fn is_null_mask(self) -> Column[bool]` | `true` where null; never null itself |
| `map` | `fn map[U: ColumnType](self, f: own MutFn(ref T) -> U) -> Column[U]` | `f` applied to each valid slot; a null slot stays null and `f` is not called for it. The baked `ElementwiseMap.map` is `T -> T` only |
| `zip_with` | `fn zip_with[U: ColumnType, V: ColumnType](self, other: Column[U], f: own MutFn(ref T, ref U) -> V) -> Column[V]` | Pairwise; null where either side is null. Panics if the lengths differ |
| `filter` | `fn filter(self, mask: Column[bool]) -> Column[T]` | The slots where `mask` is `true`; a null mask slot drops its row, as SQL `WHERE` does. Panics if the lengths differ |
| `sorted` | `fn sorted(self; descending: bool = false) -> Column[T]` | Needs `T: PartialOrd`. Stable; nulls last. NaN sorts after every number and before null |
| `argsort` | `fn argsort(self; descending: bool = false) -> Vec[i64]` | The order `sorted` would use |
| `unique` | `fn unique(self) -> Column[T]` | Needs `T: Eq + Hash`, or a float. Distinct values in order of first appearance; one null if any |

### Operators

Arithmetic operators are ordinary operator-trait impls ([design.md §9](../design.md#9-traits)), so `a + b` on two columns works as it does on any user type with an `Add` impl. Each impl borrows its operands, as `String`'s `Add` does, so neither column is moved. The result is a new column; each slot is null where either input slot is null.

| Form | Impl | Result |
|---|---|---|
| `a ⊕ b`, column with column | `impl[T: Numeric] Add for Column[T]` (and `Sub`, `Mul`, `Div`) | `Column[T]`. Panics if the lengths differ |
| `a ⊕ x`, column with scalar | `impl[T: Numeric] Add[T] for Column[T]` | `Column[T]`. `x` applies to every slot |
| `x ⊕ a`, scalar with column | `impl Add[Column[f64]] for f64`, one impl per numeric primitive and operator | `Column[f64]`, so `2.0 * col` and `1.0 - col` read as written. The orphan rule admits these impls because `Column` appears in the trait's type argument ([design.md](../design.md#coherence-and-the-orphan-rule)) |
| `-a` | `impl[T: Numeric] Neg for Column[T]` | `Column[T]` |

The literal rule of [design.md § Numeric literals](../design.md#numeric-literals) applies to the element type: `prices * 2` on a `Column[f64]` takes `2` as `f64`, and `counts + 1` on a `Column[i32]` takes `1` as `i32`. Mixed element types need a conversion first: `ints.map(|x| *x as f64)`.

Integer arithmetic is checked, as it is on scalars: an overflow or an integer division by zero in any valid slot panics. Float arithmetic follows IEEE 754 (`x / 0.0` is infinity or NaN).

**Decision Q5.** Pandas and Polars write a null where an integer division is by zero, so a single bad row does not abort an ETL job. Kāra panics, as checked scalar `i64` division does, and a program that wants nulls filters the zero rows first.

`%` is not defined on columns; `a.zip_with(b, |x, y| *x % *y)` covers it.

**Comparisons are methods, not operators.** `PartialEq.eq` and `PartialOrd.lt` return `bool`, so `a > 2.0` cannot yield a `Column[bool]` through a trait impl, and the library is written in Kāra on top of the traits ([README](README.md#how-the-library-is-built)). The element-wise comparisons are methods instead, each with a column or a scalar on the right:

| Method | Signature |
|---|---|
| `eq`, `ne`, `lt`, `le`, `gt`, `ge` | `fn gt[R: CompareArg[T]](self, rhs: R) -> Column[bool]` |

`CompareArg[T]` is implemented for `T` and for `Column[T]`. Each result slot is null where either input is null. `==` and `<` on two columns are the whole-value comparisons of `Vec`: `a == b` is `true` when the lengths, the bitmaps and the valid values are all equal.

**Decision Q6.** The legacy compiler typed `a > 2.0` as `Column[bool]` with a built-in rule, and `deferred.md`'s planner example wrote `col("age") > 21` on an `Expr`. Both are methods instead, as in Polars. An operator form would need a compiler rule outside the operator traits, the one place in the library that would not be ordinary Kāra; adding one later is a language decision, not a library change.

**Masks.** A `Column[bool]` combines with methods, under Kleene logic, as SQL does: `null and false` is `false`, `null or true` is `true`, every other combination with a null is null.

| Method | Signature |
|---|---|
| `and`, `or` | `fn and(self, rhs: Column[bool]) -> Column[bool]` |
| `not` | `fn not(self) -> Column[bool]` |
| `any`, `all` | `fn any(self) -> bool`, over the valid slots; `all` of an all-null column is `true` |
| `count_true` | `fn count_true(self) -> i64` |

`and`, `or` and `not` are keywords as operators but parse as method names (measured on v2); `&` and `|` stay refused on `bool` and are not defined on masks.

### Reductions and statistics

Every reduction reads only the valid slots. A reduction with nothing to reduce returns `None` instead of trapping, because an empty group is a normal event in a report and an ETL job should not die on one.

| Method | Signature | Notes |
|---|---|---|
| `sum` | `fn sum(self) -> T` | Needs `T: Numeric`. `0` for no valid slots. Integer overflow panics |
| `prod` | `fn prod(self) -> T` | `1` for no valid slots |
| `min`, `max` | `fn min(self) -> Option[T]` | Needs `T: PartialOrd + Copy`. A NaN slot propagates: the result is NaN |
| `argmin`, `argmax` | `fn argmin(self) -> Option[i64]` | The index of the first minimum |
| `mean` | `fn mean(self) -> Option[f64]` | Needs `T: Numeric`; integers are summed exactly, then divided |
| `median` | `fn median(self) -> Option[f64]` | The mean of the two middle values for an even count |
| `quantile` | `fn quantile(self, q: f64) -> Option[f64]` | `q` in `[0, 1]`, linear interpolation between closest ranks (NumPy's and pandas' default). Panics if `q` is outside `[0, 1]` or NaN |
| `var` | `fn var(self) -> Option[f64]` | **Sample** variance, divisor `n − 1`. `None` for fewer than two valid values |
| `std` | `fn std(self) -> Option[f64]` | `sqrt(var())` |
| `corr` | `fn corr(self, other: Column[f64]) -> Option[f64]` | On `Column[f64]` only. Pearson, over the rows where both are valid; `None` for fewer than two; NaN when either side has zero variance. Panics if the lengths differ |
| `count` | `fn count(self) -> i64` | Same as `valid_count` |
| `n_unique` | `fn n_unique(self) -> i64` | Distinct valid values |

`var` and `std` are the sample forms and `Stats.variance` and `Stats.stddev` the population forms, deliberately; the table and the reason are [deferred.md § Variance divisor](../deferred.md#column--nullable-1d-data). `quantile` takes `q` in `[0, 1]` and `Stats.percentile` takes `p` in `[0, 100]`, also as their models do. A program that wants the population variance of a column writes `Stats.variance(col.dropna().as_slice().unwrap())`.

**Decision Q7.** The baked surface returns a bare `f64` from every reduction and trapped where there was no answer. The reductions with no answer on an empty input return `Option`, and `var` and `std` return `None` for a single value; `sum` and `prod` stay bare, since their empty answer is the identity.

### Conversion

| Method | Signature | Notes |
|---|---|---|
| `as_slice` | `fn as_slice(self) -> Option[Slice[T]]` | The values buffer as a view, when the column has no nulls; `None` otherwise. The bridge to `Stats` and to `Tensor` |
| `to_arrow_ipc` | `fn to_arrow_ipc(self) -> Vec[u8]` | A one-field Arrow IPC stream ([Arrow IPC](#arrow-ipc)) |
| `from_arrow_ipc` | `fn from_arrow_ipc(bytes: Slice[u8]) -> Result[Column[T], DataError]` | `Err(TypeMismatch)` when the field's type is not `T`'s; `Err(Arrow)` for a malformed stream. The baked surface returns `Column[T]` and cannot report either |

`Column[T]` implements `Clone`, `Display` (the first and last few slots, with `null` for a null slot), and `PartialEq` when `T` does. It is `CrossTask` when `T` is, so a column can be sent to a task or across a channel.

**Decision Q8.** A column owns its buffers, so `clone`, `select`, `column` and `lazy` copy what they return: `select` and `column` cost O(rows) for each column they return. The legacy interpreter shared buffers between a frame and these results; Polars does the same by reference-counting immutable Arrow buffers. That becomes the representation, with `clone` O(1) and `push` copying first when a buffer is shared, once v2 has an atomically reference-counted buffer the library can build.

## `DataFrame`

A frame's columns all have the same length, its **height**; its **width** is the number of columns. Column names are unique and keep their insertion order, which is the schema order of the Arrow mapping, the CSV header order and the `Display` order.

### Construction

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new() -> DataFrame` | No columns, height 0 |
| `insert` | `fn insert[T: ColumnType](mut ref self, name: Str, col: own Column[T]) -> Result[(), DataError]` | Appends a column. `Err(DuplicateColumn)` if the name is taken; `Err(LengthMismatch)` if the frame has columns and `col`'s length differs |
| `with_column` | `fn with_column[T: ColumnType](own self, name: Str, col: own Column[T]) -> Result[DataFrame, DataError]` | The same as `insert` on a frame it consumes, for chaining. A taken name is replaced in place |
| `read_csv` | `fn read_csv(path: Str; options) -> Result[DataFrame, DataError] with reads(FileSystem)` | See [CSV](#csv) |
| `parse_csv` | `fn parse_csv(text: Str; options) -> Result[DataFrame, DataError]` | The same parser over text already in memory: a request body, a pipe, a test fixture |
| `read_arrow`, `from_arrow_ipc` | See [Arrow IPC](#arrow-ipc) | |

**Decision Q9.** Building a frame from a `Vec` of records (`DataFrame.from_rows(orders)` for a `struct Order` that derives `Serialize`) is the most natural ETL entry point after CSV. It comes in a later release, after Serialize settles on v2.

### Reading columns

| Method | Signature | Notes |
|---|---|---|
| `height`, `width` | `fn height(self) -> i64` | |
| `column_names` | `fn column_names(self) -> Vec[String]` | In schema order |
| `has_column` | `fn has_column(self, name: Str) -> bool` | |
| `schema` | `fn schema(self) -> Schema` | Each column's `Field { name: String, data_type: DataType, nullable: bool }`; `nullable` is whether the column has a null |
| `column` | `fn column[T: ColumnType](self, name: Str) -> Result[Column[T], DataError]` | `T` comes from the destination: `let ms: Column[i64] = df.column("ms")?;`, since a call takes no type arguments. `Err(NoSuchColumn)`, or `Err(TypeMismatch)` naming the column's real type |

A wrong `T` is an `Err` at run time, not a compile error: a frame read from a file has the schema of the file, which the compiler cannot see. The baked surface trusted the annotation and could misread the buffer.

### Selecting and reshaping

Each returns a new frame and leaves the receiver as it was.

| Method | Signature | Notes |
|---|---|---|
| `select` | `fn select(self, names: Slice[Str]) -> Result[DataFrame, DataError]` | The named columns, in the order given. `Err(NoSuchColumn)` |
| `drop` | `fn drop(self, names: Slice[Str]) -> Result[DataFrame, DataError]` | Every column but these |
| `rename` | `fn rename(self, from: Str, to: Str) -> Result[DataFrame, DataError]` | `Err(NoSuchColumn)` or `Err(DuplicateColumn)` |
| `head` | `fn head(self, n: i64) -> DataFrame` | The first `n` rows, or all of them |
| `slice` | `fn slice(self, start: i64, len: i64) -> DataFrame` | Panics if the range is out of bounds |
| `with_columns` | `fn with_columns(self, exprs: Slice[Expr]) -> Result[DataFrame, DataError]` | Each expression evaluated over the frame and added (or replaced) under its output name |
| `vstack` | `fn vstack(self, other: DataFrame) -> Result[DataFrame, DataError]` | `other`'s rows appended. `Err(SchemaMismatch)` unless the names and types agree in order |

### Filtering and sorting

| Method | Signature | Notes |
|---|---|---|
| `filter` | `fn filter(self, pred: Expr) -> Result[DataFrame, DataError]` | The rows where `pred` is `true`. A null drops the row |
| `filter_mask` | `fn filter_mask(self, mask: Column[bool]) -> Result[DataFrame, DataError]` | The same with a mask computed from columns. `Err(LengthMismatch)` |
| `sort_by` | `fn sort_by(self, names: Slice[Str]; descending: bool = false) -> Result[DataFrame, DataError]` | Stable, lexicographic over the keys; nulls last |
| `unique` | `fn unique(self; subset: Option[Slice[Str]] = None) -> Result[DataFrame, DataError]` | Rows distinct over `subset` (default all columns), keeping each first occurrence in order |

### Grouping and aggregation

```kara
let report = orders
    .group_by(["region", "month"])
    .agg([
        col("amount").sum().alias("revenue"),
        col("order_id").count().alias("orders"),
        col("amount").mean().alias("avg_order"),
    ])?
    .sort_by(["region", "month"])?;
```

| Method | Signature | Notes |
|---|---|---|
| `group_by` | `fn group_by(self, keys: Slice[Str]) -> GroupBy` | |
| `GroupBy.agg` | `fn agg(own self, aggs: Slice[Expr]) -> Result[DataFrame, DataError]` | One row per group: the key columns, then one column per aggregate, named by its `alias` or by its input column |
| `GroupBy.count` | `fn count(own self) -> Result[DataFrame, DataError]` | The keys and a `count` column of rows per group |

**Groups come out in the order their keys first appear in the input.** `Map` and `Set` iterate in a per-process random order ([collections.md](collections.md)), and a report whose row order changes between runs cannot be diffed, cached or tested; first-appearance order costs one index per group. Sort the result for any other order.

Nulls in a key column form one group, as SQL's `GROUP BY` does. An aggregate over a group whose input column is all null is a null slot, never a trap: `sum` gives null rather than `0` there, as SQL does.

### Joins

```kara
let enriched = orders.join(customers, ["customer_id"], how: JoinKind.Left)?;
```

| Method | Signature | Notes |
|---|---|---|
| `join` | `fn join(self, other: DataFrame, on: Slice[Str]; how: JoinKind = JoinKind.Inner, suffix: Str = "_right") -> Result[DataFrame, DataError]` | `Err(NoSuchColumn)` if a key is missing on either side; `Err(TypeMismatch)` if a key's types differ |

```kara
enum JoinKind { Inner, Left, Semi, Anti }
```

- `Inner`: a row for each pair of rows whose keys are equal.
- `Left`: also each left row with no match, its right columns null.
- `Semi`: the left rows that have a match, left columns only.
- `Anti`: the left rows that have none, left columns only. This is "orders with no customer", the usual data-quality check.

The output has the key columns once, then the left frame's other columns, then the right's; a right column whose name the left already has gets `suffix`. Rows come out in left order, and a left row's matches in right order, so the result is deterministic. A null key matches nothing, as in SQL.

**Decision Q10.** Outer and cross joins, and joins on differently named keys (`left_on`, `right_on`), are left out of the first release.

### Summary and printing

| Method | Signature | Notes |
|---|---|---|
| `describe` | `fn describe(self) -> DataFrame` | A `statistic` column (`count`, `null_count`, `mean`, `std`, `min`, `25%`, `50%`, `75%`, `max`), then one `Column[f64]` per numeric input column. A statistic with no answer is null. The baked surface has no `null_count` row and writes NaN for a missing `std` |

`DataFrame` implements `Display` as a text table: the header, then at most the first 10 and last 10 rows, with `null` for a null slot and a final line of the shape (`[1204 rows x 5 columns]`). It implements `Clone` and `PartialEq`, and is `CrossTask`, since every element type is.

## Expressions

An `Expr` names a computation over a frame's columns; it is evaluated by `filter`, `with_columns`, `agg` and the `LazyFrame` methods. Column types are checked when it is evaluated, since a frame's schema is known only at run time, so a wrong type is an `Err(TypeMismatch)` from that call.

| Item | Signature | Notes |
|---|---|---|
| `col` | `fn col(name: Str) -> Expr` | A free function of `data`, so that queries read like Polars' |
| `lit` | `fn lit[T: ColumnType](value: T) -> Expr` | A constant; rarely needed, since the methods take scalars |
| `eq`, `ne`, `lt`, `le`, `gt`, `ge` | `fn gt[R: IntoExpr](own self, rhs: R) -> Expr` | `IntoExpr` is implemented for `Expr` and for each element type, so `col("status").ge(500)` needs no `lit` |
| `add`, `sub`, `mul`, `div` | `fn add[R: IntoExpr](own self, rhs: R) -> Expr` | Also the operators: `impl Add for Expr` and `impl[T: ColumnType] Add[T] for Expr`, so `col("price") * col("qty")` |
| `and`, `or`, `not` | `fn and(own self, rhs: Expr) -> Expr` | Kleene logic, as on masks |
| `is_null`, `is_not_null` | `fn is_null(own self) -> Expr` | |
| `fill_null` | `fn fill_null[R: IntoExpr](own self, value: R) -> Expr` | |
| `count`, `sum`, `mean`, `min`, `max`, `median`, `std`, `var`, `n_unique`, `first`, `last` | `fn sum(own self) -> Expr` | Aggregates; per group inside `agg`, over the whole frame elsewhere |
| `quantile` | `fn quantile(own self, q: f64) -> Expr` | |
| `alias` | `fn alias(own self, name: Str) -> Expr` | The output column's name |

A literal compared with a column takes the column's element type when it converts losslessly, by the literal rule: `col("bytes").gt(1000)` works on an `i32` column. Any other mismatch is `Err(TypeMismatch)`.

**Decision Q11.** The baked type was `LazyExpr`, with `LazyExpr.col` and `LazyExpr.lit` as associated functions and `alias_`. It is renamed `Expr`, since eager calls take it too, and makes `col` and `lit` free functions. `and_`, `or_`, `not_` and `alias_` lose their underscores, since method names may be keywords.

## `LazyFrame`

`df.lazy()` starts a plan; each method adds a step, and `collect` optimizes and runs it. The first release's optimizer pushes filters and projections toward the scan, folds constants and removes repeated sub-expressions ([deferred.md § Lazy DataFrame Query Planner](../deferred.md#lazy-dataframe-query-planner)). A lazy query's result equals the eager chain's, rows and order alike.

| Method | Signature | Notes |
|---|---|---|
| `lazy` | `fn lazy(self) -> LazyFrame` | On `DataFrame`. The plan holds its own copy of the frame (Decision Q8) |
| `scan_csv` | `fn scan_csv(path: Str; options) -> LazyFrame` | A plan that reads the file at `collect`, reading only the columns the plan uses |
| `select` | `fn select(own self, exprs: Slice[Expr]) -> LazyFrame` | |
| `filter` | `fn filter(own self, pred: Expr) -> LazyFrame` | |
| `with_columns` | `fn with_columns(own self, exprs: Slice[Expr]) -> LazyFrame` | |
| `group_by` | `fn group_by(own self, keys: Slice[Expr]) -> LazyGroupBy` | `LazyGroupBy.agg(own self, aggs: Slice[Expr]) -> LazyFrame` |
| `join` | `fn join(own self, other: LazyFrame, on: Slice[Str]; how: JoinKind = JoinKind.Inner, suffix: Str = "_right") -> LazyFrame` | |
| `sort_by` | `fn sort_by(own self, keys: Slice[Expr]; descending: bool = false) -> LazyFrame` | |
| `limit` | `fn limit(own self, n: i64) -> LazyFrame` | |
| `collect` | `fn collect(own self) -> Result[DataFrame, DataError]` | Every error the eager chain would report, reported here. A `scan_csv` plan has `reads(FileSystem)` here |
| `explain` | `fn explain(self) -> String` | The optimized plan as text |

**Decision Q12.** A `scan_csv` plan reads the file only at `collect`, so `collect`'s effect depends on how the plan was started. `collect` has `reads(FileSystem)` always, which over-approximates for an in-memory plan; the alternative was a second type, `LazyScan`, carrying the effect.

## CSV

| Function | Signature |
|---|---|
| `read_csv` | `fn read_csv(path: Str; delimiter: char = ',', header: bool = true, null_value: Str = "", infer: Infer = Infer.All) -> Result[DataFrame, DataError] with reads(FileSystem)` |
| `parse_csv` | `fn parse_csv(text: Str; ...) -> Result[DataFrame, DataError]`, with `read_csv`'s options |
| `write_csv` | `fn write_csv(self, path: Str; delimiter: char = ',') -> Result[(), DataError] with writes(FileSystem)` |
| `to_csv` | `fn to_csv(self; delimiter: char = ',') -> String` |
| `scan_csv` | On `LazyFrame`, with `read_csv`'s options |

`enum Infer { All, First(i64) }` chooses how many rows type inference reads.

- **Format.** RFC 4180: a quoted field may hold the delimiter, a newline or a doubled quote. A row with a different number of fields from the header is `Err(Csv { line, message })`, naming the 1-based line. Without a header, columns are named `column_1`, `column_2`, ….
- **Types.** Each column takes the first type in the order `bool`, `i64`, `f64`, `String` that every non-null cell parses as. `bool` is exactly `true` or `false`. `Infer.All` reads every row before choosing, so a type is never discovered wrong halfway through a file; `Infer.First(n)` looks at `n` rows and makes a later cell that does not parse an `Err`.
- **Nulls.** A cell equal to `null_value` is null in every column type, the empty cell by default. An empty `String` therefore reads back as null, which `write_csv` also writes for a null: the round trip cannot tell them apart.
- **Writing.** Cells print as `println` prints them (floats shortest round-trip); a null is `null_value`; a field is quoted only when it holds the delimiter, a quote or a newline.

**Decision Q13.** The baked reader inferred `i64 → f64 → String` with no `bool` step and returned `IoError`, so a parse failure and a missing file shared one error type. The reader adds `bool` and the named options, and reports through `DataError`. An explicit schema argument, to skip inference or force `String` for zip codes and ids with leading zeros, is the next option ETL needs.

## Arrow IPC

Arrow IPC is how a frame reaches Python (pyarrow, pandas, Polars), DuckDB and the rest of the Arrow ecosystem without a text round trip. The in-memory layout is Arrow's ([deferred.md § Memory Layout Commitments](../deferred.md#memory-layout-commitments-arrow)), so writing a batch copies buffers out without converting them.

| Method | Signature | Notes |
|---|---|---|
| `to_arrow_ipc` | `fn to_arrow_ipc(self) -> Vec[u8]` | The **stream** format, one record batch: `pyarrow.ipc.open_stream(buf).read_all()` |
| `from_arrow_ipc` | `fn from_arrow_ipc(bytes: Slice[u8]) -> Result[DataFrame, DataError]` | Every batch of a stream, concatenated |
| `write_arrow` | `fn write_arrow(self, path: Str) -> Result[(), DataError] with writes(FileSystem)` | The **file** format (Feather v2): `pyarrow.feather.read_table(path)` or `duckdb.read_arrow` |
| `read_arrow` | `fn read_arrow(path: Str) -> Result[DataFrame, DataError] with reads(FileSystem)` | |

Each element type maps to the Arrow type in [Element types](#element-types), and the validity bitmap to Arrow's null buffer. A reader also accepts LargeUtf8 as `String`. A field of any other type (dictionary-encoded, list, struct, timestamp), or a compressed buffer, is `Err(Unsupported)` naming the field.

**Decision Q14.** The baked `from_arrow_ipc` widened Int32 and Float32 to `i64` and `f64`. With all eight integer widths in [Element types](#element-types), each width is kept as written.

## `Stats`

`Stats` holds statistics over a dense `Slice[f64]`, for numbers already in a `Vec` with no nulls to skip. A `Vec[f64]` passes as `Slice[f64]`; a column passes through `as_slice` or `dropna`.

| Function | Signature | Notes |
|---|---|---|
| `sum`, `prod` | `fn sum(xs: Slice[f64]) -> f64` | `0.0` and `1.0` for an empty slice |
| `mean` | `fn mean(xs: Slice[f64]) -> Option[f64]` | |
| `median` | `fn median(xs: Slice[f64]) -> Option[f64]` | |
| `percentile` | `fn percentile(xs: Slice[f64], p: f64) -> Option[f64]` | `p` in `[0, 100]`, linear interpolation; `percentile(xs, 50.0)` is `median(xs)`. Panics if `p` is outside `[0, 100]` |
| `variance` | `fn variance(xs: Slice[f64]) -> Option[f64]` | **Population** variance, divisor `n`; `0.0` for one element |
| `stddev` | `fn stddev(xs: Slice[f64]) -> Option[f64]` | `sqrt(variance(xs))` |
| `min`, `max` | `fn min(xs: Slice[f64]) -> Option[f64]` | |
| `argmin`, `argmax` | `fn argmin(xs: Slice[f64]) -> Option[i64]` | The first index |
| `sort`, `argsort` | `fn sort(xs: Slice[f64]) -> Vec[f64]` | Ascending, stable, NaN last |

Every function is `None` on an empty slice where the baked surface traps. A NaN element propagates to the result of every function but `sort` and `argsort`.

**Decision Q15.** The baked typechecker also accepted `Slice[i64]` with element-typed results. `Stats` stays at `f64`, since an integer column already has the full `Column` surface; generic `Stats` waits for `Numeric`.

## Errors

```kara
enum DataError {
    NoSuchColumn(String),
    DuplicateColumn(String),
    TypeMismatch { column: String, expected: DataType, found: DataType },
    LengthMismatch { expected: i64, found: i64 },
    SchemaMismatch(String),
    Csv { line: i64, message: String },
    Arrow(String),
    Unsupported(String),
    Io(IoError),
}
```

`DataError` implements `Error` and `Display`, and `From[IoError]`, so `?` passes a file error through.

## Effects

Every function and method has `allocates(Heap)`. The file functions add `reads(FileSystem)` or `writes(FileSystem)`, as their signatures show; nothing else touches a resource. A "panics if" note marks the calls that can panic: out-of-range indexes, a length mismatch between a column and a column or mask, a quantile outside its range, and integer overflow or division by zero in arithmetic. Every condition that a file, a schema or a user's query can cause is an `Err` instead.

## Decisions

The calls this page made where the earlier design or the baked surface was silent or different, settled with the spec owner on 2026-10-28.

| | Question | Decided |
|---|---|---|
| Q1 | Date and timestamp element types in the first release? | No; such cells read as `String` |
| Q2 | Rename `from_iter_nullable` to `from_options`? | Yes |
| Q3 | Is `c[i]` a form? `Index.index` returns `ref Output`, so it cannot return `Option[ref T]` for a null slot | No; `get(i)` |
| Q4 | `2.0 * col` needs `impl Mul[Column[f64]] for f64` in `kara-data`, which defines neither `Mul` nor `f64` | The orphan rule admits an impl when the package defines a type that appears anywhere in the trait's type arguments ([design.md](../design.md#coherence-and-the-orphan-rule)) |
| Q5 | Integer division by zero in a column: panic or null? | Panic, as checked scalar arithmetic does |
| Q6 | Element-wise comparison operators (`a > 2.0` as `Column[bool]`, `col("x") > 21` as `Expr`) need a rule outside the operator traits. Keep them? | No; methods. An operator rule later is a language decision |
| Q7 | Reductions with no answer return `Option` instead of trapping? | Yes, `var` and `std` at one value included; deferred.md's divisor table says so too |
| Q8 | Columns own their buffers (copy on `clone`, `select`, `column`, `lazy`) or share reference-counted immutable buffers (O(1))? | Own, until v2 has an atomically reference-counted buffer |
| Q9 | `DataFrame.from_rows` over `Serialize` records in the first release? | Later |
| Q10 | Outer and cross joins, `left_on` / `right_on`? | Later |
| Q11 | Rename `LazyExpr` to `Expr`, with free `col` and `lit` | Yes |
| Q12 | The effect of `LazyFrame.collect` on a `scan_csv` plan | `reads(FileSystem)` always |
| Q13 | CSV inference adds `bool`; errors are `DataError`; schema override next | Yes |
| Q14 | Arrow reads keep each integer and float width | Yes |
| Q15 | `Stats` stays `f64`-only | Yes |
| Q16 | The import path of a package named `kara-data`, whose hyphen is not an identifier character | A hyphen maps to `_` (`kara_data`), and a manifest may declare another import name; `kara-data` declares `data` ([design.md §4](../design.md#4-modules-and-packages)) |
