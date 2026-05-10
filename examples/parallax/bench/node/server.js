// Slice E — Node reference impl for the three-language Parallax bench.
//
// `http` (Node stdlib, no Express) + `Promise.all` for fan-out.
// Single-process per F4 — README footnotes that cluster mode would
// scale roughly linearly with worker count at the cost of process
// orchestration.
//
// **Sleep substitute.** Per the F5 design lock, providers should
// approximate 2/5/8/12 ms latency. Kāra's stdlib has no `sleep_ms`
// so its impl uses CPU-bound busy loops; Node mirrors the busy-loop
// shape (not `setTimeout`) at the same iteration counts so the four
// impls stay apples-to-apples. README footnotes the deviation.
//
// **`BOUND_PORT=<n>` line.** Mirrors Kāra's runtime convention so
// `bench.sh` can use one port-discovery helper across all four impls.

'use strict';

const http = require('http');

const FETCH_PROFILE_WORK = 700000;
const FETCH_ORDERS_WORK = 4000000;
const FETCH_NOTIFS_WORK = 1700000;
const FETCH_RECOMMEND_WORK = 2700000;

// Hash-mix kernel: a step V8's optimizer cannot reduce to closed
// form (no algebraic identity for `(x*31 + i) mod p`). Replaces the
// predecessor triangular-sum kernel which JIT codegen pattern-
// matched to constant-time arithmetic. See
// `docs/investigations/bench_robustness.md § G1`. Same kernel + same
// constants as the Kāra, Rust, and Go impls so the four impls
// measure equivalent work. `Math.trunc` keeps `x` an integer through
// the modulo since JS only has `Number` (no integer type); for the
// values involved (< 2^31), Number's 53-bit mantissa is plenty of
// headroom.
function busyLoop(n) {
  let x = 1;
  for (let i = 0; i < n; i++) {
    x = (x * 31 + i) % 1073741789;
  }
  return x;
}

async function fetchProfileName(userId) {
  busyLoop(FETCH_PROFILE_WORK + userId);
  return 'Alice';
}

async function fetchLatestOrderId(userId) {
  return busyLoop(FETCH_ORDERS_WORK + userId);
}

async function fetchTopNotificationKind(userId) {
  return busyLoop(FETCH_NOTIFS_WORK + userId);
}

async function fetchTopRecommendationId(userId) {
  return busyLoop(FETCH_RECOMMEND_WORK + userId);
}

async function getDashboard(userId) {
  // Promise.all is Node's idiomatic fan-out + join. With single-process
  // event loop + CPU-bound busy loops, the four awaits resolve serially
  // on the same thread — the parallelism is async-task scheduling, not
  // real cores. F4: this is the honest single-process default.
  const [profileName, orderId, notifKind, recId] = await Promise.all([
    fetchProfileName(userId),
    fetchLatestOrderId(userId),
    fetchTopNotificationKind(userId),
    fetchTopRecommendationId(userId),
  ]);
  return {
    profile: { user_id: userId, name: profileName },
    latest_order: { order_id: orderId },
    top_notification: { kind: notifKind },
    top_recommendation: { item_id: recId },
  };
}

const server = http.createServer(async (req, res) => {
  void req.url;
  const d = await getDashboard(1);
  const body = JSON.stringify(d);
  res.statusCode = 200;
  res.setHeader('content-type', 'application/json');
  res.end(body);
});

server.listen(0, '127.0.0.1', () => {
  const addr = server.address();
  process.stdout.write(`BOUND_PORT=${addr.port}\n`);
});
