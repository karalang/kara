// Smoke test of the server the extension launches: start the real `kara-lsp`
// binary over stdio, exactly as the extension does, and check the three v1
// features the extension exists for: diagnostics, go-to-definition and the
// handshake capabilities behind hover/formatting. Run with `npm run test:lsp`
// after `cargo build -p kara-lsp` (or set KARA_LSP to a binary).

"use strict";

const assert = require("assert");
const cp = require("child_process");
const fs = require("fs");
const path = require("path");
const rpc = require("vscode-jsonrpc/node");

const repo = path.resolve(__dirname, "..", "..", "..");

function serverPath() {
  if (process.env.KARA_LSP) return process.env.KARA_LSP;
  for (const profile of ["release", "debug"]) {
    const p = path.join(repo, "target", profile, process.platform === "win32" ? "kara-lsp.exe" : "kara-lsp");
    if (fs.existsSync(p)) return p;
  }
  throw new Error("no kara-lsp binary: run `cargo build -p kara-lsp` or set KARA_LSP");
}

const uri = "file:///tmp/smoke.kara";
const good = [
  "fn double(x: i64) -> i64 {",
  "    x * 2",
  "}",
  "",
  "fn main() {",
  "    let n = double(21);",
  "    println(f\"{n}\");",
  "}",
  "",
].join("\n");
const bad = good.replace("double(21)", "double(\"no\")");

async function main() {
  const bin = serverPath();
  const child = cp.spawn(bin, [], { stdio: ["pipe", "pipe", "pipe"] });
  let stderr = "";
  child.stderr.on("data", (d) => (stderr += d));
  const conn = rpc.createMessageConnection(
    new rpc.StreamMessageReader(child.stdout),
    new rpc.StreamMessageWriter(child.stdin),
  );
  const diagnostics = [];
  let wake = () => {};
  conn.onNotification("textDocument/publishDiagnostics", (p) => {
    diagnostics.push(p);
    wake();
  });
  conn.listen();
  const nextDiagnostics = () =>
    new Promise((resolve, reject) => {
      const t = setTimeout(() => reject(new Error("no publishDiagnostics within 30 s")), 30000);
      wake = () => {
        clearTimeout(t);
        resolve(diagnostics[diagnostics.length - 1]);
      };
    });

  const init = await conn.sendRequest("initialize", { processId: process.pid, rootUri: null, capabilities: {} });
  const caps = init.capabilities;
  for (const c of ["definitionProvider", "hoverProvider", "documentFormattingProvider", "documentSymbolProvider"]) {
    assert.ok(caps[c], `server does not advertise ${c}`);
  }
  await conn.sendNotification("initialized", {});

  let pending = nextDiagnostics();
  await conn.sendNotification("textDocument/didOpen", {
    textDocument: { uri, languageId: "kara", version: 1, text: good },
  });
  let d = await pending;
  assert.strictEqual(d.uri, uri);
  assert.deepStrictEqual(d.diagnostics, [], `clean program got diagnostics: ${JSON.stringify(d.diagnostics)}`);

  pending = nextDiagnostics();
  await conn.sendNotification("textDocument/didChange", {
    textDocument: { uri, version: 2 },
    contentChanges: [{ text: bad }],
  });
  d = await pending;
  assert.ok(d.diagnostics.length > 0, "a type error produced no diagnostic");
  assert.strictEqual(d.diagnostics[0].range.start.line, 5, `diagnostic on the wrong line: ${JSON.stringify(d.diagnostics[0])}`);

  pending = nextDiagnostics();
  await conn.sendNotification("textDocument/didChange", {
    textDocument: { uri, version: 3 },
    contentChanges: [{ text: good }],
  });
  d = await pending;
  assert.deepStrictEqual(d.diagnostics, [], "fixing the error did not clear the diagnostic");

  // `double` in `let n = double(21);` (line 5) goes to `fn double` (line 0).
  const def = await conn.sendRequest("textDocument/definition", {
    textDocument: { uri },
    position: { line: 5, character: 13 },
  });
  const loc = Array.isArray(def) ? def[0] : def;
  assert.ok(loc, "no definition for `double`");
  assert.strictEqual(loc.uri, uri);
  assert.strictEqual(loc.range.start.line, 0, `definition on the wrong line: ${JSON.stringify(loc)}`);

  const hover = await conn.sendRequest("textDocument/hover", {
    textDocument: { uri },
    position: { line: 1, character: 4 },
  });
  assert.ok(hover && JSON.stringify(hover.contents).includes("i64"), `hover on \`x\` lacks its type: ${JSON.stringify(hover)}`);

  await conn.sendRequest("shutdown");
  await conn.sendNotification("exit");
  const code = await new Promise((resolve) => child.on("exit", resolve));
  assert.strictEqual(code, 0, `kara-lsp exited ${code}; stderr:\n${stderr}`);
  conn.dispose();
  console.log("kara-lsp smoke: initialize, diagnostics (clean, error, cleared), definition, hover, shutdown: ok");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
