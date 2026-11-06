#!/usr/bin/env bash
# Measure the open re-bench rows that only an arm64 (M5) machine can settle,
# each on its own metric, MIR against legacy dd86e3955 (or against itself at
# another worker count), and print one table with a verdict against 1.15x.
#
#   bash scripts/bench/m5-rows.sh                 # from the kara checkout
#
# Environment (all optional):
#   KATAS=<dir>          kara-katas checkout (default ../kara-katas)
#   LEGACY_KARAC=<path>  a karac built at dd86e3955 (default: the worktree
#                        ../kara-legacy-dd86, created and built if missing)
#   REPS=<n>             runs per measurement (default 5; min for counters
#                        and wall time, median for RSS)
#   SKIP_BUILD=1         use target/release/karac and its archives as they are
#   ONLY=<substring>     measure only the rows whose id or kata contains it
#
# Counters come from scripts/pmc.c on macOS (libproc: whole-process
# instructions and cycles, as the rows' M5 figures were taken) and from
# scripts/pmc_linux.c elsewhere (needs a real PMU; a VM without one prints
# n/a). RSS is /usr/bin/time -l's maximum resident set size on macOS and
# getrusage's ru_maxrss elsewhere. Every pair's stdout is compared: a DIFF in
# the sink column voids that row's verdict.
#
# The table is also appended to target/m5-rows-<sha>.tsv, one dated block per
# run, so a loop over ONLY= values collects into one file; paste that file back.
set -euo pipefail

KARA="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$KARA"
KATAS="${KATAS:-$KARA/../kara-katas}"
REPS="${REPS:-5}"
OUT="$KARA/target/m5-rows"
mkdir -p "$OUT"

[ -d "$KATAS/leetcode" ] || { echo "kara-katas not found at $KATAS (set KATAS=...)" >&2; exit 1; }

build_runtime() {
    # Lean first, then the full archive takes the canonical name (CLAUDE.md § Runtime archives).
    cargo rustc -p karac-runtime --release --no-default-features --features net --crate-type staticlib
    cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a
    cargo rustc -p karac-runtime --release --crate-type staticlib
}

if [ "${SKIP_BUILD:-}" != 1 ]; then
    echo "== building karac (MIR) at $(git rev-parse --short HEAD)" >&2
    cargo build --release --features llvm >&2
    build_runtime >&2
fi
KARAC="$KARA/target/release/karac"

LEGACY_KARAC="${LEGACY_KARAC:-}"
if [ -z "$LEGACY_KARAC" ]; then
    LDIR="$KARA/../kara-legacy-dd86"
    if [ ! -x "$LDIR/target/release/karac" ]; then
        echo "== building legacy dd86e3955 in $LDIR (one time)" >&2
        [ -d "$LDIR" ] || git worktree add --detach "$LDIR" dd86e3955
        (cd "$LDIR" && cargo build --release --features llvm && build_runtime) >&2
    fi
    LEGACY_KARAC="$LDIR/target/release/karac"
fi

case "$(uname -s)" in
    Darwin) PMC_SRC=scripts/pmc.c ;;
    *) PMC_SRC=scripts/pmc_linux.c ;;
esac
PMC="$OUT/pmc"
cc -O2 -o "$PMC" "$PMC_SRC"

export KARAC LEGACY_KARAC KATAS REPS OUT PMC
SHA="$(git rev-parse --short HEAD)"
export SHA
exec python3 - "$@" <<'PY'
import os, statistics, subprocess, sys, time, platform

KARAC, LEGACY = os.environ["KARAC"], os.environ["LEGACY_KARAC"]
KATAS, OUT, PMC = os.environ["KATAS"], os.environ["OUT"], os.environ["PMC"]
REPS, SHA = int(os.environ["REPS"]), os.environ["SHA"]
ONLY = os.environ.get("ONLY", "")
BAR = 1.15
MAC = platform.system() == "Darwin"

# (row, kata dir, bench source, kind, metric, note)
#  pair:     seq MIR (KARAC_AUTO_PAR=0) vs seq legacy
#  threads:  MIR default build at KARAC_THREADS=a vs =b (ratio a/b)
#  par:      default (auto-par) MIR vs default legacy
#  base:     default MIR against a fixed baseline the row gives
#  seqfix:   seq MIR against the row's own figure before its fix (a source
#            legacy cannot compile); within when at most half of it
#  rust:F    seq MIR against the kata's Rust mirror bench/F, built with
#            rustc -O (a source legacy cannot compile, measured against Rust)
ROWS = [
    ("B-2026-10-31-37", "1-100/38-count-and-say", "count_and_say.kara", "pair", "cycles", ""),
    ("B-2026-11-01-17", "1-100/71-simplify-path", "simplify.kara", "threads:2:1", "wall", "2 workers vs 1"),
    ("B-2026-11-01-18", "1-100/87-scramble-string", "scramble_string.kara", "pair", "instructions", "x86 1.054x"),
    ("B-2026-11-01-22", "101-200/116-populating-next-right-pointers-in-each-node", "connect.kara", "pair", "cycles", ""),
    ("B-2026-11-01-42", "101-200/147-insertion-sort-list", "insertion_sort_list.kara", "pair", "cycles", ""),
    ("B-2026-11-01-43", "101-200/164-maximum-gap", "maximum_gap.kara", "pair", "instructions", "x86 1.06x"),
    ("B-2026-11-01-44", "101-200/170-two-sum-iii-data-structure-design", "two_sum_iii.kara", "pair", "cycles", ""),
    ("B-2026-11-02-3", "201-300/242-valid-anagram", "valid_anagram.kara", "pair", "cycles", ""),
    ("B-2026-11-02-7", "201-300/252-meeting-rooms", "meeting_rooms.kara", "pair", "instructions", "x86 0.97x"),
    ("B-2026-11-02-7", "201-300/253-meeting-rooms-ii", "min_meeting_rooms.kara", "pair", "instructions", "x86 1.02x"),
    ("B-2026-11-02-10", "201-300/276-paint-fence", "paint_enum.kara", "base:25.51", "wall", "vs the 08-28 feed's 25.51 ms"),
    ("B-2026-11-01-24", "101-200/124-binary-tree-maximum-path-sum", "max_path_sum.kara", "pair", "rss", "fixed 498a852019"),
    ("B-2026-11-02-8", "201-300/270-closest-binary-search-tree-value", "bst_close.kara", "threads:4:1", "rss", "fixed 2939cedca0; 4 workers vs 1"),
    ("B-2026-11-02-24", "1-100/38-count-and-say", "count_and_say.kara", "pair", "size", "fixed ad984ef96b"),
    ("B-2026-11-02-24", "201-300/282-expression-add-operators", "exprops_seq.kara", "pair", "size", "fixed ad984ef96b"),
    ("B-2026-11-02-23", "1-100/71-simplify-path", "simplify.kara", "par", "wall", "fixed 791fe05de0; par lane"),
    # Legacy dd86 refuses 297's, 330's, 331's, 332's and 336's sources: their
    # MIR seq builds are held to the row's own M5 bytes before the fix.
    ("B-2026-11-04-31", "201-300/297-serialize-and-deserialize-binary-tree", "treecodec.kara", "seqfix:319576", "size", "fixed 5984ed3080"),
    ("B-2026-11-04-31", "301-400/315-count-of-smaller-numbers-after-self", "count_smaller.kara", "pair", "size", "fixed 5984ed3080"),
    ("B-2026-11-04-31", "301-400/330-patching-array", "patching_array.kara", "seqfix:336128", "size", "fixed 5984ed3080"),
    ("B-2026-11-04-31", "301-400/331-verify-preorder-serialization-of-a-binary-tree", "verify_preorder.kara", "seqfix:319568", "size", "str_join fix"),
    ("B-2026-11-04-31", "301-400/332-reconstruct-itinerary", "reconstruct_itinerary.kara", "seqfix:319800", "size", "fixed 5984ed3080"),
    ("B-2026-11-04-31", "301-400/336-palindrome-pairs", "palindrome_pairs.kara", "seqfix:336320", "size", "fixed 5984ed3080"),
    ("B-2026-11-04-31", "301-400/394-decode-string", "decode_string.kara", "pair", "size", "fixed 5984ed3080"),
    ("B-2026-11-06-2", "301-400/346-moving-average-from-data-stream", "moving_average.kara", "seqfix:336240", "size", "fixed 8f6325386f"),
    # -32's bar is the kata's own Rust mirror (`rust:<file>`, rustc -O), as
    # legacy dd86 refuses 297's source; at kara-katas d6d1a5e8e or later,
    # where the Kara arm borrows its tokens as the mirror does.
    ("B-2026-11-04-32", "201-300/297-serialize-and-deserialize-binary-tree", "treecodec.kara", "rust:treecodec.rs", "rss", "x86: 31.8 vs 25.7 MiB"),
    ("B-2026-11-04-32", "201-300/297-serialize-and-deserialize-binary-tree", "treecodec.kara", "rust:treecodec.rs", "wall", "x86: Rust's time"),
    ("B-2026-11-04-38", "3601-3700/3629-minimum-jumps-to-reach-end-via-prime-teleportation", "bfs_sieve.kara", "pair", "rss", "fixed d273d89f37"),
    ("B-2026-11-04-39", "301-400/337-house-robber-iii", "house_robber_iii.kara", "pair", "instructions", "x86 1.32x -> 1.04x"),
    ("B-2026-11-04-39", "301-400/333-largest-bst-subtree", "largest_bst.kara", "pair", "instructions", "x86 1.12x -> 1.05x"),
    ("B-2026-11-04-39", "301-400/343-integer-break", "integer_break.kara", "pair", "instructions", "x86 1.14x -> 1.08x"),
    ("B-2026-11-04-39", "301-400/338-counting-bits", "counting_bits.kara", "pair", "instructions", "x86 1.15x -> 1.10x; ..= to B"),
    ("B-2026-11-04-39", "301-400/321-create-maximum-number", "create_max_number.kara", "pair", "instructions", "x86 1.12x, not fixed"),
    ("B-2026-11-04-39", "301-400/339-nested-list-weight-sum", "nested_weight_sum.kara", "pair", "instructions", "x86 1.09x, not fixed"),
    # -41's bar is rustc -O's 0.819G instructions on the M5, not legacy's.
    ("B-2026-11-04-41", "301-400/344-reverse-string", "reverse_string.kara", "pair", "instructions", "x86 LLVM 22: 3.60G -> 0.25G"),
    # -4 (teemo, fixed ce0cf3209b) and -27: x86 LLVM 22 ran these 2-3x slower than LLVM 18 at about the same
    # instruction count; cycles say whether the M5 shares it.
    ("B-2026-11-05-4", "401-500/495-teemo-attacking", "teemo_attacking.kara", "pair", "cycles", "x86 LLVM 22 357 ms vs 18's 126; fixed ce0cf3209b: 133"),
    ("B-2026-11-05-27", "101-200/155-min-stack", "min_stack.kara", "pair", "cycles", "x86 LLVM 22 900 ms vs 18's 423"),
    ("B-2026-11-05-27", "201-300/275-h-index-ii", "hsearch.kara", "pair", "cycles", "x86 LLVM 22 925 ms vs 18's 471"),
    # M5-filed rows the x86 container cannot settle (callgrind ratios in the notes).
    ("B-2026-11-04-30", "301-400/394-decode-string", "decode_string.kara", "pair", "instructions", "x86 0.96x"),
    ("B-2026-11-04-37", "301-400/342-power-of-four", "power_of_four.kara", "pair", "cycles", "x86 instr 1.11x"),
    ("B-2026-11-04-33", "301-400/314-binary-tree-vertical-order-traversal", "vertical.kara", "pair", "cycles", "x86 instr 1.07x"),
    # -1: an integer `cmp` lowered as one scmp (d70e6d474a); M5 filed 1.46x legacy's cycles, 2.05x rustc -O.
    ("B-2026-11-06-1", "301-400/354-russian-doll-envelopes", "russian_doll.kara", "pair", "cycles", "x86 0.70s -> 0.53s; fixed d70e6d474a"),
    ("B-2026-11-06-1", "301-400/354-russian-doll-envelopes", "russian_doll.kara", "rust:russian_doll.rs", "cycles", "x86 0.53s vs rustc -O 0.52s"),
    ("B-2026-11-04-33", "301-400/318-maximum-product-of-word-lengths", "max_product.kara", "pair", "instructions", "x86 1.19x: write_word not inlined"),
    ("B-2026-11-04-33", "1601-1700/1665-minimum-initial-energy-to-finish-tasks", "greedy.kara", "pair", "instructions", "x86 1.29x"),
    ("B-2026-11-05-10", "1-100/73-set-matrix-zeroes", "set_matrix_zeroes.kara", "par", "wall", "x86 8.7 s -> 0.44 s; par lane"),
]

def log(*a):
    print(*a, file=sys.stderr, flush=True)

built = {}
warned = []
def build(compiler, src, seq):
    key = (compiler, src, seq)
    if key in built:
        return built[key]
    tag = ("mir" if compiler == KARAC else "leg") + ("_seq" if seq else "_par")
    exe = os.path.join(OUT, f"{os.path.basename(src)[:-5]}_{tag}")
    env = dict(os.environ)
    if seq:
        env["KARAC_AUTO_PAR"] = "0"
    else:
        env.pop("KARAC_AUTO_PAR", None)
    r = subprocess.run([compiler, "build", src, "-o", exe], cwd=OUT, env=env,
                       capture_output=True, text=True)
    if r.returncode != 0 or not os.path.exists(exe):
        log(f"   build failed: {compiler} {src}\n{r.stderr[-600:]}")
        exe = None
    built[key] = exe
    return exe

def run_once(exe, env_extra):
    env = dict(os.environ, **env_extra)
    env.setdefault("KARAC_HASH_SEED", "1")
    cmd = [PMC, exe]
    rss = None
    if MAC and os.path.exists("/usr/bin/time"):
        cmd = ["/usr/bin/time", "-l"] + cmd
    t0 = time.perf_counter()
    p = subprocess.run(cmd, env=env, capture_output=True, text=True)
    wall = (time.perf_counter() - t0) * 1000.0
    m = {"wall": wall, "stdout": p.stdout, "ok": p.returncode == 0}
    for tok in p.stderr.split():
        if "=" in tok:
            k, v = tok.split("=", 1)
            if k in ("instructions", "cycles") and v.isdigit():
                m[k] = int(v)
    if "instructions" not in m and not warned:
        warned.append(1)
        log("   no counters from " + PMC + ": " + (p.stderr.strip().splitlines() or ["(nothing)"])[0][:200])
    for line in p.stderr.splitlines():
        if "maximum resident set size" in line:
            rss = int(line.split()[0])
    if rss is None and not MAC:
        # getrusage of a fresh child, in KiB on Linux.
        q = subprocess.run([sys.executable, "-c",
            "import resource,subprocess,sys;subprocess.run(sys.argv[1:],stdout=subprocess.DEVNULL);"
            "print(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss*1024)", exe],
            env=env, capture_output=True, text=True)
        rss = int(q.stdout.strip() or 0) or None
    m["rss"] = rss
    return m

def measure(exe, env_extra):
    runs = [run_once(exe, env_extra) for _ in range(REPS)]
    out = {"stdout": runs[0]["stdout"], "ok": all(r["ok"] for r in runs)}
    for k in ("instructions", "cycles", "wall"):
        vals = [r[k] for r in runs if k in r]
        out[k] = min(vals) if vals else None
    vals = [r["rss"] for r in runs if r["rss"]]
    out["rss"] = statistics.median(vals) if vals else None
    out["size"] = os.path.getsize(exe)
    return out

def fmt(metric, v):
    if v is None:
        return "n/a"
    if metric in ("instructions", "cycles"):
        return f"{v/1e6:.1f}M"
    if metric == "wall":
        return f"{v:.1f}ms"
    if metric == "rss":
        return f"{v/1048576:.1f}MiB"
    if metric == "size":
        return f"{v/1024:.0f}KiB"
    return str(v)

rows_out = []
for rid, kdir, srcname, kind, metric, note in ROWS:
    if ONLY and ONLY not in rid and ONLY not in kdir:
        continue
    src = os.path.join(KATAS, "leetcode", kdir, "bench", srcname)
    kata = kdir.split("/")[1].split("-")[0]
    log(f"== {rid} kata {kata} {srcname}: {kind} {metric}")
    a = b = None
    sink = "same"
    if kind == "pair":
        ea, eb = build(KARAC, src, True), build(LEGACY, src, True)
        if ea and eb:
            a, b = measure(ea, {}), measure(eb, {})
    elif kind.startswith("threads:"):
        _, x, y = kind.split(":")
        e = build(KARAC, src, False)
        if e:
            a, b = measure(e, {"KARAC_THREADS": x}), measure(e, {"KARAC_THREADS": y})
    elif kind == "par":
        ea, eb = build(KARAC, src, False), build(LEGACY, src, False)
        if ea and eb:
            a, b = measure(ea, {}), measure(eb, {})
    elif kind.startswith("rust:"):
        # The kata's Rust mirror, built with rustc -O, is the bar.
        rs = os.path.join(os.path.dirname(src), kind.split(":", 1)[1])
        er = os.path.join(OUT, os.path.basename(rs)[:-3] + "_rust")
        r = subprocess.run(["rustc", "-O", "-o", er, rs], capture_output=True, text=True)
        e = build(KARAC, src, True)
        if r.returncode != 0:
            log(f"   rustc failed: {rs}\n{r.stderr[-600:]}")
        elif e:
            a, b = measure(e, {}), measure(er, {})
    elif kind.startswith("seqfix:"):
        e = build(KARAC, src, True)
        if e:
            a = measure(e, {})
            b = {metric: float(kind.split(":")[1]), "stdout": a["stdout"], "ok": True}
    elif kind.startswith("base:"):
        e = build(KARAC, src, False)
        if e:
            a = measure(e, {})
            b = {metric: float(kind.split(":")[1]), "stdout": a["stdout"], "ok": True}
    va = a.get(metric) if a else None
    vb = b.get(metric) if b else None
    if a and b and (a["stdout"] != b["stdout"] or not a["ok"] or not b["ok"]):
        sink = "DIFF" if a["stdout"] != b["stdout"] else "EXIT"
    ratio = va / vb if va and vb else None
    if ratio is None:
        verdict = "n/a"
    elif sink != "same":
        verdict = "VOID"
    else:
        verdict = "within" if ratio <= (0.5 if kind.startswith("seqfix:") else BAR) else "OVER"
    extra = ""
    if a and kind == "pair" and metric == "cycles" and a.get("instructions") and b.get("instructions"):
        extra = f"instr {a['instructions']/b['instructions']:.3f}x"
    rows_out.append((rid, kata, metric, fmt(metric, va), fmt(metric, vb),
                     f"{ratio:.3f}x" if ratio else "n/a", verdict, sink, note, extra))

hdr = ("row", "kata", "metric", "MIR", "legacy/base", "ratio", f"vs {BAR}x", "sink", "note", "also")
widths = [max(len(str(r[i])) for r in rows_out + [hdr]) for i in range(len(hdr))]
line = lambda r: "  ".join(str(c).ljust(w) for c, w in zip(r, widths)).rstrip()
KSHA = subprocess.run(["git", "-C", KATAS, "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
print(f"m5-rows at kara {SHA}, kara-katas {KSHA}, on {platform.system()} {platform.machine()}, legacy {LEGACY}, {REPS} runs each")
print(line(hdr))
for r in rows_out:
    print(line(r))
tsv = os.path.join(os.path.dirname(OUT), f"m5-rows-{SHA}.tsv")
# Append, so several ONLY= runs at one sha land in one file; each run opens
# with its own dated header line.
import datetime
with open(tsv, "a") as f:
    stamp = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    f.write(f"# m5-rows at kara {SHA}, kara-katas {KSHA}, on {platform.system()} {platform.machine()}, {REPS} runs each, {stamp}, ONLY={ONLY or '(all)'}\n")
    for r in [hdr] + rows_out:
        f.write("\t".join(str(c) for c in r) + "\n")
print(f"\nwritten: {tsv}")
PY
