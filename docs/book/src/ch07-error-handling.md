# Error Handling

Kāra has no exceptions. No try/catch. Errors are values, handled explicitly through `Result` and `Option`.

This sounds strict, but in practice it makes error handling *easier* — the compiler tracks which operations can fail and makes sure you handle them.

## Result and the ? operator

An operation that can fail returns `Result[T, E]`:

```kara
fn parse_port(s: String) -> Result[u16, ParseError] {
    // returns Ok(port) or Err(error)
}
```

The `?` operator propagates errors to the caller:

```kara
fn load_config(path: String) -> Result[Config, Error] {
    let text = read_file(path)?;       // if Err, return it immediately
    let config = parse_toml(text)?;     // same here
    Ok(config)                          // success
}
```

Without `?`, you'd need a `match` at every step. `?` keeps the happy path clean.

## Option and ?

`?` also works with `Option`:

```kara
fn first_letter(text: Option[String]) -> Option[String] {
    let t = text?;                     // if None, return None early
    if t.len() == 0 { return None; }
    Some(t.substring(0, 1))
}
```

### Optional chaining

For navigating nested optional values:

```kara
let city = user.address?.city?.name;
```

If any step is `None`, the whole expression short-circuits to `None`.

### Default values with ??

```kara
let name = user.nickname ?? "anonymous";
let port = parse_port(input) ?? 8080;
```

`??` provides a fallback when the left side is `None` or `Err`. The fallback is evaluated lazily — only when needed.

## unwrap — the escape hatch

`unwrap()` extracts the value from `Option` or `Result`, crashing if it's `None` or `Err`:

```kara
let value = some_option.unwrap();   // panics if None
```

This produces the `panics` effect — the compiler tracks it through your call chain. Public functions that can panic must declare it. For production code, prefer `?`, `??`, `unwrap_or(default)`, or `match`.

`unwrap()` is useful in tests, prototypes, and cases where you've already validated the value can't be `None`/`Err`.

## Cleanup with defer and errdefer

`defer` runs cleanup when a scope exits, regardless of how:

```kara
fn process_file(path: String) -> Result[Data, Error] {
    let file = open(path)?;
    defer file.close();          // always runs when scope exits

    let data = parse(file)?;     // if this fails, file still gets closed
    Ok(data)
}
```

`errdefer` runs cleanup only on the error path:

```kara
fn open_connection(addr: String) -> Result[Connection, Error] {
    let conn = Connection.open(addr)?;
    errdefer conn.close();       // only if we return Err below

    register_metrics(conn)?;     // if this fails, close the connection
    Ok(conn)                     // success: errdefer does NOT run
}
```

Multiple `defer`/`errdefer` blocks run in reverse order (last declared, first executed).

## The error handling philosophy

Kāra's approach comes down to:

- **Errors are data.** They flow through the type system like any other value.
- **The compiler enforces handling.** You can't silently ignore a `Result`.
- **`?` makes the happy path clean.** Error propagation is one character, not five lines of boilerplate.
- **`defer` handles cleanup.** No RAII gymnastics, no finally blocks — just "run this when we leave."

The result is code where the error paths are visible but not noisy.
