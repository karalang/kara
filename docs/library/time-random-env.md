# Time, randomness and the environment

Every source of nondeterminism enters a Kāra program through a named resource: `Clock`, `RandomSource` or `Env`. The principle, how these effects are inferred, and why observability (logging, tracing) is transparent to them are in [design.md §12](../design.md#nondeterminism-resources). This file lists the APIs that read and write these resources.

A private function that calls any of them infers the effect with no annotation; a `pub` function declares it, so its callers can see from the signature that it is nondeterministic.

## `Clock`

In `std.time`:

| Function | Effects | Returns |
|---|---|---|
| `Clock.now()` | `reads(Clock)` | An `Instant`: a reading of the monotonic clock |
| `SystemTime.now()` | `reads(Clock)` | A `SystemTime`: the current wall-clock time |
| `sleep(d: Duration)` | `blocks`, `reads(Clock)` | Nothing; parks the calling thread for `d` |

All time sources are one resource, `Clock`: wall and monotonic time differ in the API, not in conflict analysis, and two calls that both read `Clock` do not conflict.

**`Instant`** is a point on the monotonic clock, for measuring elapsed time:
- `later - earlier` is the `Duration` between two instants;
- `i + d` is the `Instant` a `Duration` `d` after `i`;
- instants compare with `==`, `<` and the other comparison operators;
- `i.elapsed()` is the `Duration` from `i` to now. It reads the clock, so it has `reads(Clock)`.

**`SystemTime`** is wall-clock time, which can jump when the system clock is set. `t.since_epoch()` is the `Duration` since the Unix epoch. Use `Instant` to measure how long something takes.

`Duration` is a span of time:

```kara
impl Duration {
    fn ms(ms: i64) -> Duration
    fn secs(secs: i64) -> Duration
    fn as_ms(self) -> i64
}
```

```kara
import std.time.{Clock, Duration, sleep};

fn backoff(attempt: i64) {
    sleep(Duration.ms(100 * attempt));
}

fn timed(work: Fn()) -> Duration {
    let start = Clock.now();
    work();
    start.elapsed()
}
```

## `RandomSource`

| Function | Effects | Returns |
|---|---|---|
| `std.random.next_u64()` | `reads(RandomSource)` | 64 random bits |
| `std.uuid.v4()` | `reads(RandomSource)` | A random (version 4) UUID |

Cryptographic nonces also read `RandomSource`. A pseudo-random generator seeded from a constant is not `reads(RandomSource)`: it is deterministic, and its state is an ordinary value passed as an argument.

## `Env`

`Env` is the process environment: environment variables, command-line arguments, the working directory, and anything else the OS hands the process at startup or on request. It is reached through the prelude alias `env`.

```kara
fn env.args() -> Vec[String] with reads(Env)
fn env.var(name: Str) -> Result[String, VarError] with reads(Env)
fn env.set_var(name: Str, value: Str) with writes(Env)
```

`env.args()` returns the command-line arguments, the program name first. `env.var(name)` returns `Err(VarError.NotPresent)` when the variable is unset and `Err(VarError.NotUnicode)` when its value is not valid UTF-8. `VarError` and its conversion into `IoError` are in [io.md](io.md#errors).

```kara
fn port() -> i64 {
    match env.var("PORT") {
        Ok(s) => i64.parse(s).unwrap_or(8080),
        Err(_) => 8080,
    }
}
```

## Testing with fixed values

Supplying a fake `Clock`, `RandomSource` or `Env` to a test, so that the code under test runs deterministically, uses providers, which come with the services track (M4a); see [deferred.md](../deferred.md#m4a-services).
