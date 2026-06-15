// verify_browser.mjs — drive the real Fathom build in headless Chrome over the
// Chrome DevTools Protocol and assert the interactive pan/zoom actually works.
//
// Node's mock-DOM harness (cf. ssr_counter/run_browser.mjs) cannot exercise
// this demo: it needs SharedArrayBuffer + a real Web Worker pool + a <canvas>,
// and — the whole point of this slice — it needs live wheel/pointer events to
// flow through the host listeners into the wasm channels. So this drives an
// actual browser: it dispatches synthetic CDP mouse events (the real input
// path, not a JS shim) and checks the rendered canvas changes in response.
//
// Requires: a recent Chrome/Chromium and node >= 22 (global WebSocket/fetch).
// Run:  ./build.sh --build && node verify_browser.mjs
// Exits 0 on PASS, 1 on failure, 2 on a missing-prerequisite skip.
//
// What it asserts:
//   1. The page loads cross-origin-isolated (SharedArrayBuffer available).
//   2. The frame counter advances — the blocking render loop is running on a
//      worker the host keeps waking (no `await`).
//   3. The canvas has real content (non-uniform pixels), not a blank frame.
//   4. A scroll-up wheel event zooms (the canvas content changes).
//   5. A pointer move pans (the canvas content changes again).

import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PORT = 8731; // static server
const CDP_PORT = 9395; // chrome remote-debugging
const PAGE_URL = `http://127.0.0.1:${PORT}/index.html`;
const HERE = new URL(".", import.meta.url).pathname;

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
      try {
        if (spawnSync(c, ["--version"]).status === 0) return c;
      } catch {}
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

// Minimal CDP client over a single WebSocket, flatten mode (sessionId per msg).
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
  send(method, params = {}, sessionId) {
    const id = ++this.id;
    const payload = { id, method, params };
    if (sessionId) payload.sessionId = sessionId;
    this.ws.send(JSON.stringify(payload));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
}

let server, chrome, userDataDir;
function cleanup() {
  try { chrome?.kill("SIGKILL"); } catch {}
  try { server?.kill("SIGKILL"); } catch {}
  try { if (userDataDir) rmSync(userDataDir, { recursive: true, force: true }); } catch {}
}
process.on("exit", cleanup);

// Overall watchdog: never let a wedged CDP await hang the run forever.
const WATCHDOG = setTimeout(() => {
  console.error(`FAIL: watchdog — verify exceeded 90s (last stage: ${lastStage})`);
  process.exit(3);
}, 90000);
let lastStage = "start";
const stage = (s) => { lastStage = s; console.error(`[stage] ${s}`); };

async function main() {
  const chromePath = findChrome();
  if (!chromePath) {
    console.error("SKIP: no Chrome/Chromium found (set $CHROME).");
    process.exit(2);
  }
  stage("serve");

  // 1. Serve the example cross-origin isolated.
  server = spawn("python3", [join(HERE, "serve.py"), String(PORT)], { stdio: "ignore" });
  if (!(await waitForHttp(PAGE_URL))) throw new Error("static server never came up");

  // 2. Launch headless Chrome with a CDP endpoint.
  userDataDir = mkdtempSync(join(tmpdir(), "fathom-cdp-"));
  chrome = spawn(chromePath, [
    "--headless=new",
    "--no-sandbox",
    "--disable-gpu",
    "--disable-dev-shm-usage",
    "--no-first-run",
    "--no-default-browser-check",
    `--user-data-dir=${userDataDir}`,
    `--remote-debugging-port=${CDP_PORT}`,
    "about:blank",
  ], { stdio: "ignore" });

  stage("cdp-endpoint");
  let version;
  for (let i = 0; i < 60; i++) {
    try {
      version = await (await fetch(`http://127.0.0.1:${CDP_PORT}/json/version`)).json();
      break;
    } catch {}
    await sleep(100);
  }
  if (!version) throw new Error("Chrome CDP endpoint never came up");

  stage("cdp-ws");
  const ws = new WebSocket(version.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener("open", res, { once: true });
    ws.addEventListener("error", rej, { once: true });
  });
  const cdp = new CDP(ws);

  // 3. Open a tab on the page and attach a flat session.
  stage("create-target");
  const { targetId } = await cdp.send("Target.createTarget", { url: PAGE_URL });
  const { sessionId } = await cdp.send("Target.attachToTarget", { targetId, flatten: true });
  await cdp.send("Page.enable", {}, sessionId);
  await cdp.send("Runtime.enable", {}, sessionId);
  stage("attached");

  const evalJs = async (expr) => {
    const r = await cdp.send("Runtime.evaluate", {
      expression: expr, returnByValue: true, awaitPromise: true,
    }, sessionId);
    if (r.exceptionDetails) throw new Error("page JS threw: " + JSON.stringify(r.exceptionDetails));
    return r.result.value;
  };

  // 4. Wait for cross-origin isolation + the render loop to start.
  stage("isolation");
  let isolated = false;
  for (let i = 0; i < 60; i++) {
    isolated = await evalJs("self.crossOriginIsolated === true");
    if (isolated) break;
    await sleep(100);
  }
  if (!isolated) throw new Error("page is NOT cross-origin isolated (no SharedArrayBuffer)");

  const frameCount = () => evalJs(
    `(() => { const m = document.getElementById('overlay').textContent.match(/frames:\\s*(\\d+)/); return m ? +m[1] : 0; })()`
  );
  // A cheap content fingerprint: sum a sparse pixel sample of the canvas.
  const fingerprint = () => evalJs(`(() => {
    const c = document.getElementById('screen');
    const g = c.getContext('2d');
    const d = g.getImageData(0, 0, c.width, c.height).data;
    let h = 0, lo = 255, hi = 0;
    for (let i = 0; i < d.length; i += 257) { h = (h * 31 + d[i]) >>> 0; lo = Math.min(lo, d[i]); hi = Math.max(hi, d[i]); }
    return h + ':' + lo + ':' + hi;
  })()`);

  // 5. Frames must advance.
  stage("frames");
  const f0 = await frameCount();
  await sleep(800);
  const f1 = await frameCount();
  if (!(f1 > f0)) throw new Error(`render loop not advancing: frames ${f0} -> ${f1}`);

  // 6. Canvas must have real content (non-uniform).
  const fp0 = await fingerprint();
  const [, lo0, hi0] = fp0.split(":").map(Number);
  if (hi0 - lo0 < 8) throw new Error(`canvas looks blank/uniform: ${fp0}`);

  // Canvas centre in *viewport* coords (the canvas is padded/centred in the
  // page, so use its rect origin, not just its size).
  const rect = await evalJs(`(() => { const r = document.getElementById('screen').getBoundingClientRect();
    return { left: r.left, top: r.top, w: r.width, h: r.height }; })()`);
  const cx = Math.round(rect.left + rect.w / 2), cy = Math.round(rect.top + rect.h / 2);

  // 7. Wheel scroll-up over the canvas centre must zoom (content changes).
  for (let i = 0; i < 6; i++) {
    await cdp.send("Input.dispatchMouseEvent", {
      type: "mouseWheel", x: cx, y: cy, deltaX: 0, deltaY: -240,
    }, sessionId);
    await sleep(120);
  }
  await sleep(400);
  const fpZoom = await fingerprint();
  if (fpZoom === fp0) throw new Error("wheel zoom did not change the canvas");

  // 8a. Hover (NO button held) must NOT pan. With click-drag gating the view is
  //     static between inputs, so a buttonless move must leave it unchanged —
  //     this is the positive proof the `buttons` gate works, not hover-pan.
  for (let i = 1; i <= 6; i++) {
    await cdp.send("Input.dispatchMouseEvent", {
      type: "mouseMoved", x: cx + i * 14, y: cy + i * 9, buttons: 0,
    }, sessionId);
    await sleep(60);
  }
  await sleep(400);
  const fpHover = await fingerprint();
  if (fpHover !== fpZoom) {
    throw new Error(`hover with no button held must NOT pan, but canvas changed: ${fpZoom} -> ${fpHover}`);
  }

  // 8b. Click-drag (primary button held) MUST pan (content changes).
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mousePressed", x: cx, y: cy, button: "left", buttons: 1, clickCount: 1,
  }, sessionId);
  for (let i = 1; i <= 8; i++) {
    await cdp.send("Input.dispatchMouseEvent", {
      type: "mouseMoved", x: cx + i * 12, y: cy + i * 8, button: "left", buttons: 1,
    }, sessionId);
    await sleep(60);
  }
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mouseReleased", x: cx + 96, y: cy + 64, button: "left", buttons: 0, clickCount: 1,
  }, sessionId);
  await sleep(500);
  const fpDrag = await fingerprint();
  if (fpDrag === fpHover) throw new Error("click-drag (primary button held) did not pan the canvas");

  // 8c. Keyboard (ArrowRight via keydown()) MUST pan (content changes). Drives
  //     the real keydown producer: CDP key event → window listener → channel →
  //     wasm recv.
  for (let i = 0; i < 6; i++) {
    const k = { key: "ArrowRight", code: "ArrowRight", windowsVirtualKeyCode: 39, nativeVirtualKeyCode: 39 };
    await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", ...k }, sessionId);
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", ...k }, sessionId);
    await sleep(100);
  }
  await sleep(400);
  const fpKey = await fingerprint();
  if (fpKey === fpDrag) throw new Error("keyboard ArrowRight (keydown producer) did not pan the canvas");

  const fEnd = await frameCount();
  console.log(`PASS — isolated, frames ${f0}->${f1}->${fEnd}, content fp ${fp0} ` +
    `--wheel--> ${fpZoom} --hover(no-pan)--> ${fpHover} --drag--> ${fpDrag} --key--> ${fpKey}`);
  ws.close();
}

main().then(() => { clearTimeout(WATCHDOG); process.exit(0); }).catch((e) => {
  clearTimeout(WATCHDOG);
  console.error("FAIL:", e.message);
  process.exit(1);
});
