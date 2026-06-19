// verify_browser.mjs — the Iris A/B proof, in a real browser.
//
// This is the payoff of "one source -> native + WASM, no port": it runs the
// NATIVE build (the checksum oracle) to get a ground-truth hash of every
// filtered image, then drives the BROWSER build in headless Chrome over the
// DevTools Protocol, switches through all six filters, reads the rendered canvas
// back, hashes it with the identical FNV-1a — and asserts every filter's browser
// pixels hash to the exact value the native binary produced. Byte-for-byte, the
// same kernel ran natively and in wasm.
//
// A node mock-DOM harness can't do this: it needs SharedArrayBuffer + a real Web
// Worker pool + a <canvas>. So this drives an actual browser.
//
// Requires: a recent Chrome/Chromium, node >= 22, and `karac` on PATH.
// Run:  ./build.sh --build && node verify_browser.mjs
// Exits 0 on PASS, 1 on failure, 2 on a missing-prerequisite skip.

import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PORT = 8741;
const CDP_PORT = 9396;
const PAGE_URL = `http://127.0.0.1:${PORT}/index.html`;
const HERE = new URL(".", import.meta.url).pathname;

const FILTER_KEYS = [
  { id: 0, name: "original", key: 49 },
  { id: 1, name: "blur", key: 50 },
  { id: 2, name: "sharpen", key: 51 },
  { id: 3, name: "edge", key: 52 },
  { id: 4, name: "invert", key: 53 },
  { id: 5, name: "grayscale", key: 54 },
];

function findChrome() {
  const candidates = [
    process.env.CHROME,
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome",
    "chromium",
    "chromium-browser",
  ].filter(Boolean);
  for (const c of candidates) {
    if (c.includes("/")) {
      try { if (spawnSync(c, ["--version"]).status === 0) return c; } catch {}
    } else {
      const r = spawnSync("which", [c]);
      if (r.status === 0) return r.stdout.toString().trim();
    }
  }
  return null;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitForHttp(url, tries = 50) {
  for (let i = 0; i < tries; i++) {
    try {
      const r = await fetch(url);
      if (r.ok || r.status === 404) return true;
    } catch {}
    await sleep(100);
  }
  return false;
}

// Run the native build and parse its per-filter checksums — the ground truth.
function nativeChecksums() {
  const karac = process.env.KARAC || "karac";
  const build = spawnSync(karac, ["build"], { cwd: HERE, encoding: "utf8" });
  if (build.status !== 0) {
    throw new Error("native `karac build` failed:\n" + (build.stderr || build.stdout));
  }
  const bin = join(HERE, "iris");
  if (!existsSync(bin)) throw new Error("native binary `iris` not found after build");
  const run = spawnSync(bin, [], { encoding: "utf8" });
  if (run.status !== 0) throw new Error("native `./iris` failed:\n" + run.stderr);
  const map = new Map();
  for (const line of run.stdout.split("\n")) {
    // "filter <name> <id> checksum <hash>"
    const m = line.match(/^filter\s+\S+\s+(\d+)\s+checksum\s+(\d+)/);
    if (m) map.set(Number(m[1]), Number(m[2]));
  }
  if (map.size !== FILTER_KEYS.length) {
    throw new Error(`expected ${FILTER_KEYS.length} native checksums, parsed ${map.size}:\n${run.stdout}`);
  }
  return map;
}

class CDP {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    ws.addEventListener("message", (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result);
      }
    });
  }
  send(method, params = {}, sessionId, timeoutMs = 8000) {
    const id = ++this.id;
    const payload = { id, method, params };
    if (sessionId) payload.sessionId = sessionId;
    this.ws.send(JSON.stringify(payload));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CDP ${method} timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (v) => { clearTimeout(timer); resolve(v); },
        reject: (e) => { clearTimeout(timer); reject(e); },
      });
    });
  }
}

let server, chrome, userDataDir;
function cleanup() {
  try { chrome?.kill("SIGKILL"); } catch {}
  try { server?.kill("SIGKILL"); } catch {}
  try { if (userDataDir) rmSync(userDataDir, { recursive: true, force: true }); } catch {}
}
process.on("exit", cleanup);

const WATCHDOG = setTimeout(() => {
  console.error(`FAIL: watchdog — verify exceeded 150s (last stage: ${lastStage})`);
  process.exit(3);
}, 150000);
let lastStage = "start";
const stage = (s) => { lastStage = s; console.error(`[stage] ${s}`); };

async function main() {
  // 0. Native ground truth first — no point driving a browser if this fails.
  stage("native-oracle");
  const native = nativeChecksums();
  console.error(`[ok] native checksums: ${[...native.entries()].map(([k, v]) => `${k}:${v}`).join(" ")}`);

  if (!existsSync(join(HERE, "iris.js")) || !existsSync(join(HERE, "iris.threads.wasm"))) {
    console.error("SKIP: browser artifacts missing — run `./build.sh --build` first.");
    process.exit(2);
  }
  const chromePath = findChrome();
  if (!chromePath) {
    console.error("SKIP: no Chrome/Chromium found (set $CHROME).");
    process.exit(2);
  }

  stage("serve");
  server = spawn("python3", [join(HERE, "serve.py"), String(PORT)], { stdio: "ignore" });
  if (!(await waitForHttp(PAGE_URL))) throw new Error("static server never came up");

  stage("chrome");
  userDataDir = mkdtempSync(join(tmpdir(), "iris-cdp-"));
  chrome = spawn(chromePath, [
    "--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
    "--no-first-run", "--no-default-browser-check",
    `--user-data-dir=${userDataDir}`, `--remote-debugging-port=${CDP_PORT}`, "about:blank",
  ], { stdio: "ignore" });

  let version;
  for (let i = 0; i < 60; i++) {
    try { version = await (await fetch(`http://127.0.0.1:${CDP_PORT}/json/version`)).json(); break; } catch {}
    await sleep(100);
  }
  if (!version) throw new Error("Chrome CDP endpoint never came up");

  const ws = new WebSocket(version.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener("open", res, { once: true });
    ws.addEventListener("error", rej, { once: true });
  });
  const cdp = new CDP(ws);

  stage("attach");
  const { targetId } = await cdp.send("Target.createTarget", { url: PAGE_URL });
  const { sessionId } = await cdp.send("Target.attachToTarget", { targetId, flatten: true });
  await cdp.send("Page.enable", {}, sessionId);
  await cdp.send("Runtime.enable", {}, sessionId);

  const retry = async (fn, label, attempts = 6) => {
    let last;
    for (let i = 0; i < attempts; i++) {
      try { return await fn(); }
      catch (e) { last = e; if (!/timed out/.test(e.message)) throw e; await sleep(300); }
    }
    throw new Error(`${label} kept timing out (${attempts} attempts): ${last?.message}`);
  };
  const evalJs = (expr) => retry(async () => {
    const r = await cdp.send("Runtime.evaluate", { expression: expr, returnByValue: true, awaitPromise: true }, sessionId);
    if (r.exceptionDetails) throw new Error("page JS threw: " + JSON.stringify(r.exceptionDetails));
    return r.result.value;
  }, "evalJs");

  stage("isolation");
  let isolated = false;
  for (let i = 0; i < 60; i++) {
    isolated = await evalJs("self.crossOriginIsolated === true");
    if (isolated) break;
    await sleep(100);
  }
  if (!isolated) throw new Error("page is NOT cross-origin isolated (no SharedArrayBuffer)");

  // FNV-1a over the full RGBA canvas — byte-for-byte identical to the Kāra
  // `checksum` in host_macos.kara (offset 2166136261, prime 16777619, mod 2^32).
  const canvasChecksum = () => evalJs(`(() => {
    const c = document.getElementById('screen');
    const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
    let h = 2166136261 >>> 0;
    for (let i = 0; i < d.length; i++) { h = Math.imul((h ^ d[i]) >>> 0, 16777619) >>> 0; }
    return h >>> 0;
  })()`);

  // For each filter: switch via a synthetic keydown (the real host-listener
  // path; synthetic avoids headless Chrome's trusted-key flood — see the Fathom
  // verify harness note), then poll the canvas hash until it equals the native
  // oracle's checksum for that filter. This single check is BOTH the liveness
  // proof (the render loop ran and painted) and the correctness proof (the wasm
  // pixels are byte-identical to native) — no separate "frames advanced" gate.
  // Iris renders on filter-change rather than every frame, so the renderer is
  // idle between switches and these CDP reads are never starved by a pegged
  // render thread (the flakiness the old continuous-render gate suffered).
  // Filter 0 (original) is painted by the loop's initial dirty render, so it is
  // already on the canvas when the loop here switches to it.
  const dispatchKey = (f) =>
    evalJs(`window.dispatchEvent(new KeyboardEvent('keydown', { key: '${f.id + 1}', keyCode: ${f.key} }))`);

  const results = [];
  for (const f of FILTER_KEYS) {
    stage(`filter:${f.name}`);
    const want = native.get(f.id);
    let got = null, matched = false;
    const fDeadline = Date.now() + 30000;
    // Re-dispatch the switch keydown every iteration: a single early dispatch can
    // race the wasm keydown listener's attachment at startup and be lost, leaving
    // the canvas on the previous filter. Re-switching to the same filter is
    // idempotent in the render loop (picked == current → no-op), so this is safe
    // and converges the moment the listener is live.
    while (Date.now() < fDeadline) {
      try { await dispatchKey(f); } catch {}
      await sleep(250);
      try { got = await canvasChecksum(); } catch { continue; }
      if (got === want) { matched = true; break; }
    }
    if (!matched) {
      throw new Error(`filter ${f.name} (id ${f.id}): browser checksum ${got} != native ${want} within 30s`);
    }
    results.push(`${f.name}=${got}`);
    console.error(`[ok] ${f.name}: browser pixels match native (${got})`);
  }

  console.log(`PASS — isolated; all 6 filters byte-identical native vs wasm: ${results.join(" ")}`);
  ws.close();
}

main().then(() => { clearTimeout(WATCHDOG); process.exit(0); }).catch((e) => {
  clearTimeout(WATCHDOG);
  console.error("FAIL:", e.message);
  process.exit(1);
});
