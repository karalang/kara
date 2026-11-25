# std_net — minimal HTTP / TLS / WebSocket examples

The smallest working programs for Kāra's in-tree network stdlib. They
use no third-party runtime dependency and compile clean with the current `karac` (`karac check examples/std_net/<file>.kara`). For the
production-scale version of this surface — 2M idle WebSocket-over-TLS
connections — see [`examples/ws_idle_holder`](../ws_idle_holder).

| File | Module | Shows |
|---|---|---|
| `http_hello.kara` | [`runtime/stdlib/http.kara`](../../runtime/stdlib/http.kara) | `Server.serve(addr, handler)` — `Fn(Request) -> Response`, no async |
| `https_hello.kara` | [`runtime/stdlib/tls.kara`](../../runtime/stdlib/tls.kara) | `Server.serve_tls(addr, cert, key, handler)` — same handler + TLS |
| `ws_echo.kara` | [io.md § WebSocket](../../docs/library/io.md#websocket) | `WebSocket.accept` + `recv` (whole messages) and `send_text`/`send_binary`, blocking style |
| `ws_echo_spawn.kara` | [io.md § WebSocket](../../docs/library/io.md#websocket) | the same echo server, one `TaskGroup` task per connection |

The free-`spawn` form the IOCP spike cites, which 0.8 refuses (E0520), is kept at
[docs/legacy/examples/std_net/ws_echo_freespawn.kara](../../docs/legacy/examples/std_net/ws_echo_freespawn.kara).

The `ws_*_client.py` scripts drive the echo servers (`python3 ws_loop_client.py 127.0.0.1 8080 <count> <concurrency>`).

No imports are needed: `Server`, `Request`, `TcpListener`, `TlsListener`,
and `WebSocket` are stdlib builtins resolved by the typechecker.

The blocking-style I/O is the point: there is no `async fn`, no
`.await`, no function coloring. The effect-driven scheduler moves the
blocking `recv`/`send`/`accept` off the par-runtime threads.
