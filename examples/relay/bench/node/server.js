// Relay bench — Node reference impl: manual `http`-module passthrough proxy.
//
// No npm dependencies, no `http-proxy` package — a hand-written passthrough
// using only Node's `http` stdlib: accept the inbound request, open an
// `http.request` to the upstream with the same method/path/headers, pipe the
// inbound body in and the upstream response back out. Single-process per the
// Parallax F4 fairness control (faithful to Node's default deployment
// reality); cluster mode would scale ~Nx at the cost of process
// orchestration — footnoted in the README.
//
// **Upstream address.** Read from `RELAY_UPSTREAM` (an `IP:port` literal),
// defaulting to `127.0.0.1:9000`. `bench.sh` exports it pointed at the shared
// upstream's discovered ephemeral port.
//
// **BOUND_PORT convention.** Binds `127.0.0.1:0` and prints `BOUND_PORT=<n>`
// so `bench.sh`'s one port-discovery helper works across all three impls.

'use strict';

const http = require('http');

function upstreamAddr() {
  return process.env.RELAY_UPSTREAM || '127.0.0.1:9000';
}

// Listen address: RELAY_BIND env var, or 127.0.0.1:0 (ephemeral loopback, the
// local-bench default). The cross-host harness sets it to a routable
// 0.0.0.0:<port> so a client on another host can reach the proxy.
function bindAddr() {
  return process.env.RELAY_BIND || '127.0.0.1:0';
}

const [upstreamHost, upstreamPortStr] = upstreamAddr().split(':');
const upstreamPort = parseInt(upstreamPortStr, 10);

const [bindHost, bindPortStr] = bindAddr().split(':');
const bindPort = parseInt(bindPortStr, 10);

// Reuse upstream connections — the apples-to-apples mirror of Go's
// ReverseProxy connection pooling and Kāra's per-connection upstream open.
const agent = new http.Agent({ keepAlive: true, maxSockets: 4096 });

const server = http.createServer((clientReq, clientRes) => {
  const options = {
    host: upstreamHost,
    port: upstreamPort,
    method: clientReq.method,
    path: clientReq.url,
    headers: clientReq.headers,
    agent,
  };
  const upstreamReq = http.request(options, (upstreamRes) => {
    clientRes.writeHead(upstreamRes.statusCode, upstreamRes.headers);
    upstreamRes.pipe(clientRes);
  });
  upstreamReq.on('error', () => {
    if (!clientRes.headersSent) {
      clientRes.statusCode = 502;
    }
    clientRes.end();
  });
  clientReq.pipe(upstreamReq);
});

server.listen(bindPort, bindHost, () => {
  const addr = server.address();
  process.stdout.write(`BOUND_PORT=${addr.port}\n`);
});
