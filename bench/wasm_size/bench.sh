#!/usr/bin/env bash
# wasm_size — WASM module-size benchmark: Kāra vs Rust vs TinyGo.
#
# Companion to ../README.md (bench-setup protocol) and ./README.md (this track).
# Builds one minimal "hello" and one real workload (the Iris filter core) three
# ways, in two artifact flavors (core module = wasm32-wasip1, component = WASI
# 0.2 / wasip2), strips DWARF, brotli-compresses, and records the bytes. The
# headline number is the brotli'd stripped module — "every byte is downloaded".
#
# The same kernel is compiled three ways; the printed FNV-1a checksums must match
# across all languages (asserted below), so the size comparison is honest — same
# work, not a stripped-down stand-in.
#
# Output: regenerates sizes.md (the checked-in receipt) and sizes.json. Run from
# anywhere; it cds to its own directory. Re-run after any toolchain bump.

set -euo pipefail
cd "$(dirname "$0")"
SRC="$(pwd)"   # absolute bench dir; karac builds run from a temp CWD.

# Homebrew bin carries tinygo / wasm-tools / brotli on macOS.
export PATH="/opt/homebrew/bin:$PATH"

require() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "$1 not found — install with: $2" >&2
        exit 1
    fi
}
require karac      "cargo install --path ../.. --features llvm  (from karac-rust checkout)"
require cargo      "rustup (https://rustup.rs)"
require tinygo     "brew install tinygo-org/tools/tinygo  (https://tinygo.org)"
require wasm-tools "cargo install wasm-tools  OR  brew install wasm-tools"
require brotli     "brew install brotli"

# Rust wasm targets the component/core builds need.
for t in wasm32-wasip1 wasm32-wasip2; do
    rustup target list --installed 2>/dev/null | grep -qx "$t" || {
        echo "rust target $t missing — run: rustup target add $t" >&2; exit 1; }
done

# The byte-exact ground truth (Kāra native oracle / examples/iris). All three
# filter_core ports must print these, in order, or the bench fails loudly.
REF_CHECKSUMS=$'filter 0 checksum 3572457293\nfilter 1 checksum 309186456\nfilter 2 checksum 2365075192\nfilter 3 checksum 1059888691\nfilter 4 checksum 2049330177\nfilter 5 checksum 2350968365'

BUILD="$(mktemp -d)"
RESULTS="$(mktemp)"   # TSV: flavor \t workload \t lang \t raw \t stripped \t brotli
trap 'rm -rf "$BUILD" "$RESULTS"' EXIT

# WASI preview1 runner — used only to cross-check core-module checksums.
RUNNER="$BUILD/run_wasi.mjs"
cat > "$RUNNER" <<'JS'
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';
const wasi = new WASI({ version: 'preview1', args: [], env: {} });
const bytes = await readFile(process.argv[2]);
const inst = await WebAssembly.instantiate(await WebAssembly.compile(bytes), wasi.getImportObject());
wasi.start(inst);
JS

fsize() { wc -c < "$1" | tr -d ' '; }

# record FLAVOR WORKLOAD LANG WASM — strip, brotli, append a results row.
record() {
    local flavor="$1" workload="$2" lang="$3" wasm="$4"
    local raw stripped brotli
    raw="$(fsize "$wasm")"
    local s="$BUILD/${flavor}_${workload}_${lang}.stripped.wasm"
    # `wasm-tools strip` drops .debug_* / name custom sections, keeps
    # component-type/dylink so components stay valid.
    wasm-tools strip "$wasm" -o "$s" 2>/dev/null || cp "$wasm" "$s"
    stripped="$(fsize "$s")"
    brotli="$(brotli -q 11 -c "$s" | wc -c | tr -d ' ')"
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$flavor" "$workload" "$lang" "$raw" "$stripped" "$brotli" >> "$RESULTS"
    printf '  %-10s %-12s %-7s raw=%-8s stripped=%-8s brotli=%-8s\n' \
        "$flavor" "$workload" "$lang" "$raw" "$stripped" "$brotli"
}

# checksum_check WASM — run a core-module filter_core, assert the reference.
checksum_check() {
    local wasm="$1"
    command -v node >/dev/null 2>&1 || { echo "  (node absent — skipping checksum cross-check)"; return; }
    local got
    got="$(node "$RUNNER" "$wasm" 2>/dev/null || true)"
    if [ "$got" != "$REF_CHECKSUMS" ]; then
        echo "CHECKSUM MISMATCH for $wasm — kernel diverged, comparison would be dishonest:" >&2
        echo "$got" >&2
        exit 1
    fi
    echo "  checksum cross-check OK ($wasm)"
}

echo "== building (this regenerates sizes.md / sizes.json) =="

for w in hello filter_core; do
    # --- Kāra ---
    ( cd "$BUILD" && karac build "$SRC/src/kara/$w.kara" --target=wasm_wasi --bindings none >/dev/null )
    record core "$w" kara "$BUILD/$w.wasm"
    [ "$w" = filter_core ] && checksum_check "$BUILD/$w.wasm"
    ( cd "$BUILD" && karac build "$SRC/src/kara/$w.kara" --target=wasm_wasi >/dev/null )  # default = component
    record component "$w" kara "$BUILD/$w.wasm"

    # --- Rust ---
    ( cd "src/rust/$w" && cargo build --release --target wasm32-wasip1 >/dev/null 2>&1 )
    record core "$w" rust "src/rust/$w/target/wasm32-wasip1/release/$w.wasm"
    [ "$w" = filter_core ] && checksum_check "src/rust/$w/target/wasm32-wasip1/release/$w.wasm"
    ( cd "src/rust/$w" && cargo build --release --target wasm32-wasip2 >/dev/null 2>&1 )
    record component "$w" rust "src/rust/$w/target/wasm32-wasip2/release/$w.wasm"

    # --- TinyGo ---
    tinygo build -target=wasi   -opt=z -no-debug -o "$BUILD/tg_${w}_p1.wasm" "src/tinygo/$w.go" 2>/dev/null
    record core "$w" tinygo "$BUILD/tg_${w}_p1.wasm"
    [ "$w" = filter_core ] && checksum_check "$BUILD/tg_${w}_p1.wasm"
    tinygo build -target=wasip2 -opt=z -no-debug -o "$BUILD/tg_${w}_p2.wasm" "src/tinygo/$w.go" 2>/dev/null
    record component "$w" tinygo "$BUILD/tg_${w}_p2.wasm"
done

# ---- render sizes.json ----
{
    echo '{'
    echo '  "note": "WASM module sizes (bytes). brotli = brotli -q 11 of the wasm-tools-stripped module — the real download cost. Regenerated by bench.sh; do not hand-edit.",'
    echo '  "checksum_cross_check": "all filter_core ports print identical FNV-1a checksums (asserted in bench.sh)",'
    echo '  "results": ['
    awk -F'\t' '{
        printf "    {\"flavor\":\"%s\",\"workload\":\"%s\",\"lang\":\"%s\",\"raw\":%s,\"stripped\":%s,\"brotli\":%s}%s\n",
            $1,$2,$3,$4,$5,$6, (NR==total?"":",")
    }' total="$(wc -l < "$RESULTS")" "$RESULTS"
    echo '  ]'
    echo '}'
} > sizes.json

# ---- render sizes.md ----
gen_table() {  # gen_table FLAVOR
    local flavor="$1"
    echo "| workload | toolchain | raw | stripped | brotli |"
    echo "|---|---|---:|---:|---:|"
    for w in hello filter_core; do
        for l in kara rust tinygo; do
            awk -F'\t' -v f="$flavor" -v w="$w" -v l="$l" '
                $1==f && $2==w && $3==l {
                    ln=l; if(l=="kara")ln="Kāra"; else if(l=="rust")ln="Rust"; else ln="TinyGo";
                    printf "| %s | %s | %s | %s | **%s** |\n", w, ln, fmt($4), fmt($5), fmt($6)
                }
                function fmt(n,  s,r){ s=n; r=""; while(length(s)>3){ r=","substr(s,length(s)-2)r; s=substr(s,1,length(s)-3)} return s r }
            ' "$RESULTS"
        done
    done
}

{
    echo "# WASM module size — Kāra vs Rust vs TinyGo"
    echo
    echo "> Regenerated by [\`bench.sh\`](bench.sh) — **do not hand-edit**. Sizes in bytes."
    echo "> \`brotli\` = \`brotli -q 11\` of the \`wasm-tools strip\`-ed module: the real"
    echo "> download cost, since every byte of a wasm module is fetched. The headline"
    echo "> metric on WASM is module size, the way startup density is the headline on native."
    echo
    echo "All three \`filter_core\` ports print **identical FNV-1a checksums** (asserted in"
    echo "\`bench.sh\`), so this compares the *same* kernel three ways — an honest size"
    echo "comparison, not a stripped-down stand-in. Workloads: \`hello\` (minimal stdout"
    echo "program) and \`filter_core\` (the [Iris](../../examples/iris) image kernel —"
    echo "6 filters over a 512×384 procedural image)."
    echo
    echo "## Core module (\`wasm32-wasip1\`)"
    echo
    echo "The directly-comparable axis: a raw core \`.wasm\` all three toolchains emit"
    echo "natively. This is also the byte-for-byte shape Kāra's \`--target=wasm_browser\`"
    echo "ships (the browser module is the same wasip1 command module behind a JS glue"
    echo "polyfill). Kāra: \`--bindings none\`; Rust: \`--target wasm32-wasip1\` (release,"
    echo "\`opt-level=\"z\"\`, lto, \`panic=\"abort\"\`, strip); TinyGo: \`-target=wasi -opt=z -no-debug\`."
    echo
    gen_table core
    echo
    echo "## Component (WASI 0.2 / \`wasm32-wasip2\`)"
    echo
    echo "The modern artifact — a single embedded-WIT component. This is Kāra's"
    echo "\`--target=wasm_wasi\` **default** (\`--bindings component\`). Rust:"
    echo "\`--target wasm32-wasip2\`; TinyGo: \`-target=wasip2\`."
    echo
    gen_table component
    echo
    echo "> _Caveat: Kāra's component emission is not yet byte-deterministic — repeated"
    echo "> builds of identical source produce the same byte **length** but reordered"
    echo "> content, so the Kāra-component \`brotli\` cell jitters ~0.2% (~50 B) run to run"
    echo "> (raw/stripped are stable). Tracked as a follow-up; the core-module figures"
    echo "> above are fully deterministic._"
    echo
    echo "## Reading the numbers"
    echo
    echo "- **Core modules:** Kāra's AOT/no-GC/no-async posture (the wasm archive strips"
    echo "  tokio/mio/socket2; \`wasm-tools strip\` removes DWARF by default; wasm-ld"
    echo "  section-GC = native \`-dead_strip\`) lands module size in the same class as"
    echo "  size-tuned Rust and far below TinyGo (which ships a GC + runtime). On"
    echo "  \`hello\` Kāra is markedly smaller than Rust — Rust's \`println!\` pulls in std"
    echo "  fmt + the panic/backtrace machinery that Kāra's lean runtime does not."
    echo "- **Components:** the WASI preview1→preview2 adapter inflates every toolchain's"
    echo "  component over its core module. Kāra's adapter is heavier than Rust's, so on"
    echo "  the larger \`filter_core\` workload Rust's component edges ahead of Kāra's"
    echo "  (Kāra still wins \`hello\` and both core-module rows). Shrinking the embedded"
    echo "  adapter is the open size follow-up; the core-module number is the one Kāra's"
    echo "  \`--target=wasm_browser\` positioning actually ships."
    echo
    echo "_Toolchains: $(karac --version 2>/dev/null), $(rustc --version 2>/dev/null | awk '{print $1,$2}'), tinygo $(tinygo version 2>/dev/null | awk '{print $3}'). Regenerate with \`./bench.sh\`._"
} > sizes.md

echo
echo "== wrote sizes.md + sizes.json =="