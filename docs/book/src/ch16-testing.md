# Testing

> *This chapter is a work in progress.*

Kāra has built-in testing support — no external framework needed.

## Unit tests

Test functions live alongside the code they test, in `_test.kara` files. Any function whose name begins with `test_` is automatically discovered and run by `karac test` — no attribute or registration required:

```kara
// math.kara
fn add(a: i64, b: i64) -> i64 {
    a + b
}

// math_test.kara
fn test_addition_works() {
    assert_eq(add(2, 3), 5);
    assert_eq(add(-1, 1), 0);
}

fn test_addition_is_commutative() {
    assert_eq(add(3, 7), add(7, 3));
}
```

## Assertions

Available everywhere as builtins:

```kara
assert(condition);              // panics if false
assert_eq(left, right);        // panics if not equal, shows both values
```

## Property-based testing

Test with randomly generated inputs by adding `#[property]` to a `test_*` function with parameters:

```kara
#[property]
fn test_sort_is_idempotent(items: Vec[i64]) {
    let sorted = items.sort();
    assert_eq(sorted.sort(), sorted);
}
```

The test runner generates many random `Vec[i64]` values via the `Arbitrary` trait and checks the property holds for each. When a failure is found, it shrinks the input to the minimal reproducing case.

## Snapshot testing

Capture output and compare against a saved baseline by adding `#[snapshot]` to a `test_*` function:

```kara
#[snapshot]
fn test_report_format() {
    let report = generate_report(sample_data());
    assert_snapshot(report);
}
```

On first run, the snapshot is saved. On subsequent runs, the output is compared. If it changed, the test fails and shows the diff. Accept new output as the baseline with `karac test --update-snapshots`.

## Running tests

```bash
karac test                    # run all tests
karac test math               # run tests whose fully-qualified ID contains "math"
karac test addition           # run tests matching a substring of the test ID
```
