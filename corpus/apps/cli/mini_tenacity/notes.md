# mini_tenacity

Retrying calls as tenacity 9.0.0 does it ("retry this until it works,
waiting longer each time"). tenacity is the retry library behind many
Python services and tools, Apache Airflow among them, and countless HTTP
and database wrappers. The port
covers:
- `Retrying` called on a function, the `@retry` decorator with its
  per-call copies, `retry_with()` and the bare `@retry`, and the
  `for attempt in Retrying(...): with attempt:` form;
- every stop: `stop_after_attempt`, `stop_after_delay`,
  `stop_before_delay`, `stop_when_event_set`, `stop_never`, and
  `stop_any`/`stop_all`, with `|` and `&`;
- every wait: `wait_fixed`, `wait_none`, `wait_random`,
  `wait_incrementing`, `wait_exponential`, `wait_random_exponential`,
  `wait_exponential_jitter`, `wait_chain` and `wait_combine`, with `+`
  and `sum()` over waits;
- every retry condition: exception types and their negations,
  `retry_unless_exception_type`, exception causes, exception messages
  and their negation, results and their negation, `retry_always`,
  `retry_never`, `retry_any`/`retry_all` with `|` and `&`, and
  `TryAgain`;
- `before_log`, `after_log` with its `sec_format`, `before_sleep_log`,
  `retry_error_callback`, `reraise`, `RetryError` with its last attempt
  and cause, and the `statistics` dict;
- two programs:
  - a webhook delivery worker. It POSTs each event in an outbox to its
    endpoint through `@retry` with exponential backoff and jitter,
    retrying connection errors, timeouts, 5xx and 429, and stopping
    after five attempts or 20 seconds. It writes a dead-letter file and
    its log to `data/out/`;
  - a backup uploader. It sends files in 4 MiB parts, each part in a
    `for attempt in Retrying(...)` loop with full jitter that stops
    before a 60 second budget would run out. A busy store raises
    `TryAgain`, a part that runs out of budget is skipped, and a
    permission error aborts its file. It writes a manifest and its log.

The run writes `data/` and then works through the 62 jobs in
`data/jobs.jsonl` and the two programs, 246 calls of the code under
retry in all. `mk.py` wrote `data/`.

`oracle.py` runs the same jobs through tenacity itself and wrote
`expected.out`. It uses the `tenacity/` package from the 9.0.0 wheel,
unchanged. The clock is fake: `time.monotonic()` reads it, and
`time.sleep()` and each call of a job's function move it on. Addresses
in reprs print as `0x?`.

The quirks are kept. Some examples:
- `wait_exponential()` with its defaults returns ints, so
  `statistics["idle_for"]` is `31`, not `31.0`, while the retry state's
  own `idle_for` is a float. `wait_incrementing(start=0)` returns the
  int 0 through `max(0, 0.0)`.
- `wait_combine` adds with Python 3.13's `sum()`, which compensates
  float rounding, so 0.1 + 0.2 + 0.3 + 1e16 + 1 - 1e16 waits 1.6
  seconds.
- `sum([w])` is `w` itself, through `__radd__` with 0, and `0 + w` is
  too, but `5 + w` builds a `wait_combine` that fails with "'int'
  object is not callable" when it first waits.
- A float `exp_base` can overflow, and then the wait is `max`:
  `MAX_WAIT`, which is 4.611686018427388e+18 seconds.
- `KeyboardInterrupt` is not an `Exception`, so the default retry lets
  it out. A `KeyError`'s message prints as its repr.
- `TryAgain` retries even under `retry_never`.
- `retry_unless_exception_type` and `retry_if_not_exception_message`
  also retry on any result.
- An empty `message` counts as no message, so a `match` beside it is
  used.
- `reraise=True` on a result raises the `RetryError` itself.
- The iterator form's attempts return None, so `retry_if_result(is
  None)` retries a body that worked. With a `retry_error_callback`,
  giving up just ends the loop.
- `RetryCallState`'s repr rounds `slept for` with `round(x, 2)`, so
  0.125 shows as 0.12 and 1.2499 shows as 1.25.

Not ported: `AsyncRetrying` and the tornado integration, the
`exc_info` option of `before_sleep_log`, and regex patterns in
`retry_if_exception_message(match=...)`, which here are plain prefixes.

## License

tenacity is under the Apache License 2.0 in `LICENSE-tenacity.txt`:

    Copyright 2016-2018 Julien Danjou
    Copyright 2017 Elisey Zanko
    Copyright 2016 Étienne Bersac
    Copyright 2016 Joshua Harlow
    Copyright 2013-2014 Ray Holder

That covers `tenacity/`.
