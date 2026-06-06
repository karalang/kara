# std_net — minimal HTTP / TLS / WebSocket examples

The smallest working programs for Kāra's in-tree network stdlib. All
three use no third-party runtime dependency and compile clean with the
current `karac` (`karac check examples/std_net/<file>.kara`). For the
production-scale version of this surface — 2M idle WebSocket-over-TLS
connections — see [`examples/ws_idle_holder`](../ws_idle_holder).

| File | Module | Shows |
|---|---|---|
| `http_hello.kara` | [`runtime/stdlib/http.kara`](../../runtime/stdlib/http.kara) | `Server.serve(addr, handler)` — `Fn(Request) -> Response`, no async |
| `https_hello.kara` | [`runtime/stdlib/tls.kara`](../../runtime/stdlib/tls.kara) | `Server.serve_tls(addr, cert, key, handler)` — same handler + TLS |
| `ws_echo.kara` | [`runtime/stdlib/ws.kara`](../../runtime/stdlib/ws.kara) | `WebSocket.accept` + `recv_text`/`send_text`, blocking style |

No imports are needed: `Server`, `Request`, `TcpListener`, `TlsListener`,
and `WebSocket` are stdlib builtins resolved by the typechecker.

The blocking-style I/O is the point: there is no `async fn`, no
`.await`, no function coloring. The effect-driven scheduler moves the
blocking `recv`/`send`/`accept` off the par-runtime threads.
