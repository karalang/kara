// Node.js reference impl ("comparator") for the `ws_idle_holder` flagship
// demo. Mirrors `../src/main.kara` and the sibling `../go` / `../rust` /
// `../dotnet` comparators: binds a TLS listener on 127.0.0.1:<ephemeral>,
// prints `BOUND_PORT=<n>` to stdout so the `../bench` harness can read it
// back, and accepts WebSocket-over-TLS connections, echoing any frame it
// receives and holding each connection idle otherwise until the peer
// closes.
//
// # Why this exists
//
// docs/implementation_checklist/phase-6-runtime.md names the ws-idle-holder
// workload as the runtime's M1/M2/M3 scale target. Node.js (the `ws`
// library on `https`/OpenSSL) is comparator #73 in the bench-day Phase 3
// sweep — `ws` is the raw-library Node WS prod default (the "what a
// competent Node shop ships" baseline, NOT socket.io, which adds an
// engine.io transport/RPC/room layer and is a distinct framework-tier
// stretch comparator, #75). Both this server and the Kāra impl traverse
// the same kernel critical path — accept(2) -> TLS handshake -> RFC 6455
// upgrade — so the comparison isolates language-runtime overhead from the
// IO substrate.
//
// # Design choices (vs. the Kāra impl)
//
//   - Concurrency model: Node's single-threaded libuv event loop. Unlike
//     Go's goroutine-per-connection or the JVM/.NET thread-pool models,
//     Node multiplexes every connection over ONE OS thread running a
//     callback-driven reactor — architecturally the closest of all the
//     comparators to Kāra's own event-loop reactor (runtime/src/
//     event_loop.rs) and to tokio's single-runtime model in the Rust
//     comparator. A real Node shop at very high conn counts often runs the
//     `cluster` module (one worker process per core) behind a load
//     balancer, but that is a core-scaling / throughput choice, not a
//     density one: each worker holds its share of connections at the same
//     per-conn cost, and the harness measures one process's RSS. So a
//     single process is both the honest per-process density measure and
//     the apples-to-apples basis here (every comparator is measured as a
//     single OS process).
//   - WS upgrade: the `ws` library's WebSocketServer drives the RFC 6455
//     server-side handshake off the `https` server's 'upgrade' event.
//     Equivalent to the Kāra runtime's hand-rolled
//     ws_drive_upgrade_handshake, gorilla's Upgrader, and the Rust
//     comparator's tokio-tungstenite accept_async.
//   - TLS: in-process `https.createServer` over Node's bundled **OpenSSL
//     3.x**, minVersion TLS 1.2 (max defaults to TLS 1.3), no client auth,
//     single self-signed cert. This is the SAME OpenSSL substrate as the
//     .NET-on-Linux comparator (#71) — and unlike Go's pure-Go crypto/tls
//     or Kāra's rustls — which makes the Node-vs-.NET-Linux pair a clean
//     read on runtime overhead over a shared TLS stack. In-process TLS is
//     the apples-to-apples basis (every comparator terminates TLS
//     in-process). The same committed fixture (CN=localhost) is read from
//     cert.pem / key.pem next to this file, mirroring the .NET comparator.
//   - perMessageDeflate: false — set explicitly (and `ws`'s own default,
//     since a per-conn zlib context is expensive). No compression context
//     is allocated per connection, matching every other comparator (none
//     compress); this keeps the per-conn density number honest.
//   - No optional native addons. `ws` can optionally load `bufferutil` /
//     `utf-8-validate` (C++ addons) for faster masking / UTF-8 validation;
//     they are NOT installed here, so the build is pure-JS and the rig
//     needs no native toolchain. They affect CPU, not per-conn density, so
//     omitting them keeps the density comparison honest and the install
//     dependency-free.
//   - Listen backlog: Node's net.Server.listen default backlog is 511. The
//     bench rig's ../bench/scripts/ec2_setup.sh raises
//     net.core.somaxconn=65535, but Node — unlike Go — does not auto-read
//     somaxconn, so we pass an explicit backlog of 65535 to listen() to
//     match the Rust comparator's socket2 listen(65535) and Go's
//     somaxconn-derived backlog on the rig.
//   - TCP_NODELAY: Node enables it by default on all TCP sockets
//     (socket.setNoDelay(true) is the default), matching the Rust
//     comparator's explicit set_nodelay(true) — the WS upgrade response
//     goes out without Nagle delay.
//   - Cert + key: read from the committed fixture files cert.pem / key.pem
//     next to this script (resolved via __dirname), the same self-signed
//     test fixtures (CN=localhost, valid through 2036) at
//     tests/fixtures/tls/. The .NET comparator reads sibling PEM files the
//     same way; the Go comparator inlines the identical bytes because
//     //go:embed can't reach a parent dir. All three expose nothing not
//     already committed in the repo.
//
// # Echo on message (Phase 2 parity)
//
// The per-connection 'message' handler echoes any text/binary frame
// straight back, mirroring ../src/main.kara's handle_connection and the
// other comparators. This lets the active-traffic bench drive
// request/response load through every impl identically. Idle connections
// send nothing, so the echo branch is never reached on an idle hold — the
// per-connection density numbers are unchanged, and the same script serves
// both idle and active loads (no cross-binary confound).
//
// # What this impl deliberately omits
//
//   - No structured logging (a bad-handshake / TLS error must not crash the
//     process or spam the harness's stderr channel — 'clientError' on the
//     https server and 'error' on each socket are swallowed).
//   - No graceful shutdown / max-conn cap.
//   - No connection-attempt rate limiting.
//   - No `cluster` / multi-process fan-out (single process — see the
//     concurrency-model note above).

'use strict';

const https = require('node:https');
const fs = require('node:fs');
const path = require('node:path');
const { WebSocketServer } = require('ws');

// Self-signed test cert + key (CN=localhost, valid through 2036) — the same
// bytes committed at tests/fixtures/tls/{cert,key}.pem and inlined by
// ../src/main.kara. Read from sibling files (resolved via __dirname), the
// same pattern as the .NET comparator's AppContext.BaseDirectory lookup.
const cert = fs.readFileSync(path.join(__dirname, 'cert.pem'));
const key = fs.readFileSync(path.join(__dirname, 'key.pem'));

// In-process Kestrel-equivalent: an https server over Node's bundled
// OpenSSL. minVersion TLS 1.2; maxVersion defaults to TLS 1.3.
// requestCert defaults to false -> NoClientCert, matching rustls
// with_no_client_auth and the other comparators.
const server = https.createServer({
  cert,
  key,
  minVersion: 'TLSv1.2',
});

// A bad TLS handshake or malformed pre-upgrade request surfaces as
// 'clientError' on the https server; swallow it so a handshake storm can't
// crash the process or pollute the harness channel (the "no structured
// logging" omission). Default behavior would write a 400 and destroy the
// socket, which is fine — we just suppress the throw/log.
server.on('clientError', (_err, socket) => {
  socket.destroy();
});

// WebSocketServer in attached mode: it hooks the https server's 'upgrade'
// event and drives the RFC 6455 handshake. perMessageDeflate:false -> no
// per-conn zlib context (see design note).
const wss = new WebSocketServer({ server, perMessageDeflate: false });

wss.on('connection', (ws) => {
  // Echo any text/binary frame straight back, preserving the frame's
  // binary-ness. ping/pong/close control frames are handled by `ws`
  // internally. An idle connection emits no 'message', so this handler
  // never fires on an idle hold — density-neutral.
  ws.on('message', (data, isBinary) => {
    ws.send(data, { binary: isBinary });
  });
  // Swallow per-conn socket errors (e.g. ECONNRESET on abrupt peer close)
  // so they don't bubble to an uncaughtException and kill the process.
  ws.on('error', () => {});
});

// Bind on 127.0.0.1:0 to get an ephemeral port, with an explicit 65535
// backlog to match the rig's somaxconn (Node does not auto-read somaxconn).
// listen(port, host, backlog, cb).
server.listen(0, '127.0.0.1', 65535, () => {
  const { port } = server.address();
  // The BOUND_PORT convention the bench harness reads from the spawned
  // process's stdout (same as the Kāra runtime and every comparator).
  process.stdout.write(`BOUND_PORT=${port}\n`);
});
