# Testing

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

## Running tests

```bash
karac test                    # run all tests
karac test math               # run tests whose fully-qualified ID contains "math"
karac test addition           # run tests matching a substring of the test ID
```
