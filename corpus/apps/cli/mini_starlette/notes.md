# mini_starlette

A bookshop service built on Starlette 1.6.0 (BSD), with the framework
underneath it ported from `routing.py`, `convertors.py`,
`datastructures.py`, `responses.py`, `_exception_handler.py` and the CORS,
trusted-host and server-error middleware:
- `compile_path`: the pattern text with `re.escape`, the format, each
  convertor, host templates that drop their port, and the duplicate-name
  and unknown-convertor errors. Matching backtracks the way the compiled
  regex does, so `/{a}{b}`, `/{a:path}/{b:path}` and the uuid pattern's
  optional dashes split the same way;
- `url_path_for` through Routes, named Mounts (with and without `path`), a
  nested Mount and a Host, each convertor's `to_string` with its
  assertions (`%0.20f` for floats), and `NoMatchFound`'s message;
- the Router: first full match, else first partial match (a 405 whose
  Allow header lists the method set in its own iteration order),
  `redirect_slashes` at every level, `root_path` and `app_root_path`
  under Mounts, and a Host route per tenant;
- `init_headers`' header order, `JSONResponse`'s compact rendering,
  `RedirectResponse`'s quoting, `set_cookie` through `SimpleCookie` with
  its octal quoting, `cookie_parser`, `QueryParams`, `URL(scope=)` with
  `_HOST_RE` and `make_absolute_url`;
- `HTTPException` with the default phrase, a 404 status handler, a
  custom exception handler, 204 and 304 without a body, and
  `ServerErrorMiddleware`'s 500 that is raised on to the server;
- `CORSMiddleware` with credentials (preflights and their failures,
  `Vary` merged into an existing header), `TrustedHostMiddleware` with its
  redirect to `www.`, and a request-id middleware of the app's own.

A 405's Allow header joins a Python set, so its order depends on the str
hash. The oracle runs with `PYTHONHASHSEED=0`, and the port computes the
same order: SipHash-1-3 with a zero key, then CPython's set probing and
resizing.

The service keeps books in memory (list with filters and paging, create,
update, delete, prices in four currencies), and has endpoints for
converters, redirects, an echo of the request, a login that sets cookies,
errors, stock checks with a custom exception, ETags, an admin Mount and a
static-files ASGI app.

`mk.py` writes `data/`: 40 route templates, 75 `url_path_for` calls and
201 HTTP requests. `oracle.py` drives the real app through ASGI and wrote
`expected.out` (`PYTHONHASHSEED=0 python3 -s -P oracle.py`). `gen_kara.py`
prints the data section of `mini_starlette.kara`.

It passed `karac check` and matched on every backend the first time it
ran; no gaps were found. The JSON scanner's mutually recursive methods
still get the effect note that `#[allow(mutual_recursion_note)]` cannot
silence on methods (B-2026-10-20-28).
