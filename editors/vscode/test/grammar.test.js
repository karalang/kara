// Grammar checks for syntaxes/kara.tmLanguage.json, run with `npm test`.
//
// 1. Expected scopes: every `// <- scope` / `//  ^^^ scope` assertion in
//    test/fixtures/*.kara (the vscode-tmgrammar-test convention, read by the
//    small checker below) must hold.
// 2. Balance (`--corpus`, a few minutes): tokenizing every .kara program under the repo's corpus/ and
//    examples/ must end each file back at the grammar's root, so no string,
//    comment or attribute region runs away past where the compiler ends it.

"use strict";

const fs = require("fs");
const path = require("path");
const vsctm = require("vscode-textmate");
const oniguruma = require("vscode-oniguruma");

const root = path.resolve(__dirname, "..");
const repo = path.resolve(root, "..", "..");

async function loadGrammar() {
  const wasm = fs.readFileSync(require.resolve("vscode-oniguruma/release/onig.wasm"));
  await oniguruma.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
  const registry = new vsctm.Registry({
    onigLib: Promise.resolve({
      createOnigScanner: (s) => new oniguruma.OnigScanner(s),
      createOnigString: (s) => new oniguruma.OnigString(s),
    }),
    loadGrammar: async () =>
      vsctm.parseRawGrammar(
        fs.readFileSync(path.join(root, "syntaxes", "kara.tmLanguage.json"), "utf8"),
        "kara.tmLanguage.json",
      ),
  });
  return registry.loadGrammar("source.kara");
}

function tokenizeLines(grammar, text) {
  let stack = vsctm.INITIAL;
  const lines = text.split(/\r?\n/);
  const out = [];
  for (const line of lines) {
    const r = grammar.tokenizeLine(line, stack);
    out.push({ line, tokens: r.tokens });
    stack = r.ruleStack;
  }
  return { lines: out, stack };
}

function scopesAt(tokens, col) {
  const t = tokens.find((t) => t.startIndex <= col && col < t.endIndex);
  return t ? t.scopes : [];
}

// An assertion line is `//` then spaces, then either `<-` (column 0 of the
// previous source line, as many columns as there are `-`) or a run of `^`
// under the columns it covers, then one or more scopes; `- scope` negates.
function checkFixture(grammar, file) {
  const failures = [];
  const { lines } = tokenizeLines(grammar, fs.readFileSync(file, "utf8"));
  let source = null;
  let count = 0;
  lines.forEach(({ line, tokens }, i) => {
    const m = /^(\s*)\/\/(\s*)(<-+|\^+)\s+(.*)$/.exec(line);
    if (m === null || source === null) {
      if (m === null) source = { line, tokens, n: i + 1 };
      return;
    }
    let cols;
    if (m[3].startsWith("<")) {
      cols = [...Array(m[3].length - 1).keys()].map((k) => k + m[1].length);
    } else {
      const start = m[1].length + 2 + m[2].length;
      cols = [...Array(m[3].length).keys()].map((k) => k + start);
    }
    const words = m[4].trim().split(/\s+/);
    const want = [];
    const deny = [];
    for (let k = 0; k < words.length; k++) {
      if (words[k] === "-") deny.push(words[++k]);
      else want.push(words[k]);
    }
    for (const col of cols) {
      count++;
      const got = scopesAt(source.tokens, col);
      const has = (s) => got.some((g) => g === s || g.startsWith(s + "."));
      const missing = want.filter((s) => !has(s));
      const present = deny.filter(has);
      if (missing.length || present.length) {
        failures.push(
          `${path.basename(file)}:${source.n}:${col + 1} '${source.line[col]}' ` +
            `wanted [${missing.join(" ")}]${present.length ? ` must not have [${present.join(" ")}]` : ""}; ` +
            `got [${got.join(" ")}]`,
        );
      }
    }
  });
  return { failures, count };
}

function walk(dir, acc) {
  if (!fs.existsSync(dir)) return acc;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, acc);
    else if (e.name.endsWith(".kara")) acc.push(p);
  }
  return acc;
}

async function main() {
  const grammar = await loadGrammar();
  let failed = 0;

  const fixtures = walk(path.join(__dirname, "fixtures"), []);
  let assertions = 0;
  for (const f of fixtures) {
    const { failures, count } = checkFixture(grammar, f);
    assertions += count;
    for (const msg of failures) console.error("FAIL " + msg);
    failed += failures.length;
  }
  if (assertions === 0) {
    console.error("FAIL no scope assertions ran");
    failed++;
  }
  console.log(`scope assertions: ${assertions - failed} of ${assertions} hold (${fixtures.length} fixtures)`);

  if (!process.argv.includes("--corpus")) {
    finish(failed);
    return;
  }
  const programs = [...walk(path.join(repo, "corpus"), []), ...walk(path.join(repo, "examples"), [])];
  // Programs that are meant not to lex (an unterminated string or comment is
  // the point of the test) are listed in balance-allow.txt.
  const allow = new Set(
    fs
      .readFileSync(path.join(__dirname, "balance-allow.txt"), "utf8")
      .split("\n")
      .map((l) => l.trim())
      .filter((l) => l && !l.startsWith("#")),
  );
  const unbalanced = [];
  for (const p of programs) {
    const { stack } = tokenizeLines(grammar, fs.readFileSync(p, "utf8"));
    const rel = path.relative(repo, p);
    if (stack.depth !== 1 && !allow.has(rel)) unbalanced.push(rel);
  }
  for (const rel of unbalanced) console.error(`FAIL unbalanced at end of file: ${rel}`);
  failed += unbalanced.length;
  if (programs.length === 0) {
    console.error("FAIL no corpus programs found");
    failed++;
  }
  console.log(`balance: ${programs.length - unbalanced.length} of ${programs.length} programs end at the root`);
  finish(failed);
}

function finish(failed) {
  if (failed > 0) {
    console.error(`${failed} failure(s)`);
    process.exit(1);
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
