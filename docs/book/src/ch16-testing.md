# Testing

Kāra has built-in testing support — no external framework needed.

## Unit tests

Tests live alongside the code they test, in `_test.kara` files. Each test is a `test "name" { ... }` block — a quoted case name and a body. `karac test` discovers and runs every such block; no attribute or registration required:

```kara
// math.kara
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

// math_test.kara — shares module `math`'s scope, so it calls `add` directly
test "addition works" {
    assert_eq(add(2, 3), 5);
    assert_eq(add(-1, 1), 0);
}

test "addition is commutative" {
    assert_eq(add(3, 7), add(7, 3));
}
```

A `<module>_test.kara` file is part of that module, so it sees the module's
functions with no `import` — in fact importing your own module back into its
test file is a cycle error. The case name is a string, not an identifier, so it
can read like a sentence.

## Assertions

Available everywhere as builtins:

```kara
assert(condition);              // panics if false
assert_eq(left, right);        // panics if not equal, shows both values
```

## Running tests

```bash
karac test                    # run all tests
karac test addition           # run only tests whose case name contains "addition"
```

The filter is a substring of the case name — the text between `test` and `{` —
so `karac test commutative` runs just the second block above.
