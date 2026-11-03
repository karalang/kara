#!/usr/bin/env bash
# B-2026-11-01-17: where a two-worker auto-par run spends its time on macOS.
#
# Katas 71 and 125 run slower on the M5 with KARAC_THREADS=2 than with one
# worker (5x the cycles for a few % more instructions) and fast again at
# three or four; on Linux both halve cleanly from one worker to two. This
# prints, for each kata:
#   - wall and user time (min of REPS) at 1, 2, 3 and 4 workers;
#   - the same with a "padded" source that allocates a few thousand small
#     strings right after the shared input, so the per-iteration churn does
#     not land next to it in the heap (tests false sharing on the input);
#   - on macOS, `sample`'s top-of-stack summary of a longer run at 2 and at
#     4 workers (where the cycles go: allocator locks, the pool's spin, or
#     the kernel itself).
#
#   bash scripts/bench/m5-pool2.sh            # from the kara checkout
#
# Environment: REPS=<n> (default 5), SKIP_BUILD=1 to use target/release as
# it is. The benches are the corpus copies, so kara-katas is not needed.
# The report goes to target/m5-pool2-<sha>.txt; paste that file back.
set -euo pipefail

KARA="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$KARA"
REPS="${REPS:-5}"
OUT="$KARA/target/m5-pool2"
mkdir -p "$OUT"

if [ "${SKIP_BUILD:-}" != 1 ]; then
    echo "== building karac at $(git rev-parse --short HEAD)" >&2
    cargo build --release --features llvm >&2
    # Lean first, then the full archive takes the canonical name (CLAUDE.md § Runtime archives).
    cargo rustc -p karac-runtime --release --no-default-features --features net --crate-type staticlib >&2
    cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a
    cargo rustc -p karac-runtime --release --crate-type staticlib >&2
fi

KARAC="$KARA/target/release/karac" OUT="$OUT" REPS="$REPS" \
    SHA="$(git rev-parse --short HEAD)" exec python3 - <<'PY'
import os, platform, re, resource, subprocess, sys, time

KARAC, OUT, REPS, SHA = os.environ["KARAC"], os.environ["OUT"], int(os.environ["REPS"]), os.environ["SHA"]
MAC = platform.system() == "Darwin"
BALLAST = """    let mut ballast: Vec[String] = Vec.new();
    let mut b: i64 = 0;
    while b < 4096 {
        ballast.push("x".repeat(b % 64 + 1));
        b = b + 1;
    }
"""
# (kata, corpus source, the line the shared input is complete after,
#  the iteration-count line, its scaled form for the sampled run)
KATAS = [
    ("71", "corpus/katas/1-100/71-simplify-path/bench/simplify/source.kara",
     '    inputs.push("/...hidden");\n',
     "let k_iters: i64 = 1000000;", "let k_iters: i64 = 30000000;"),
    ("125", "corpus/katas/101-200/125-valid-palindrome/bench/valid_palindrome/source.kara",
     "    let input: String = base.repeat(8);\n",
     "let iters = 3000000;", "let iters = 12000000;"),
]

def log(*a):
    print(*a, file=sys.stderr, flush=True)

def build(name, text):
    src = os.path.join(OUT, name + ".kara")
    with open(src, "w") as f:
        f.write(text)
    exe = os.path.join(OUT, name)
    r = subprocess.run([KARAC, "build", src, "-o", exe], capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"build failed: {src}\n{r.stderr[-800:]}")
    return exe

def run(exe, threads):
    env = dict(os.environ, KARAC_THREADS=str(threads), KARAC_HASH_SEED="1")
    u0 = resource.getrusage(resource.RUSAGE_CHILDREN).ru_utime
    t0 = time.perf_counter()
    p = subprocess.run([exe], env=env, capture_output=True, text=True)
    wall = time.perf_counter() - t0
    user = resource.getrusage(resource.RUSAGE_CHILDREN).ru_utime - u0
    if p.returncode != 0:
        sys.exit(f"{exe} exited {p.returncode}: {p.stderr[-400:]}")
    return wall * 1000, user * 1000, p.stdout

def sample(exe, threads, path):
    env = dict(os.environ, KARAC_THREADS=str(threads), KARAC_HASH_SEED="1")
    p = subprocess.Popen([exe], env=env, stdout=subprocess.DEVNULL)
    time.sleep(0.3)
    subprocess.run(["/usr/bin/sample", str(p.pid), "2", "1", "-mayDie", "-file", path],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    p.wait()
    try:
        text = open(path).read()
    except OSError:
        return ["(sample wrote nothing)"]
    at = text.find("Sort by top of stack")
    if at < 0:
        return ["(no top-of-stack section)"]
    return [l.rstrip() for l in text[at:].splitlines()[:30]]

report = [f"m5-pool2 at kara {SHA} on {platform.system()} {platform.machine()} {platform.mac_ver()[0] if MAC else ''}, min of {REPS}"]
for kata, src, after, iters, scaled in KATAS:
    text = open(src).read()
    assert after in text and iters in text, f"kata {kata}: the bench source changed"
    padded = text.replace(after, after + BALLAST).replace("    println(sum);", "    println(sum);\n    println(ballast.len());")
    for variant, body in (("base", text), ("padded", padded)):
        exe = build(f"k{kata}_{variant}", body)
        for t in (1, 2, 3, 4):
            if variant == "padded" and t > 2:
                continue
            runs = [run(exe, t) for _ in range(REPS)]
            outs = {r[2] for r in runs}
            line = (f"kata {kata:>3} {variant:6} T={t}  wall {min(r[0] for r in runs):8.1f} ms"
                    f"  user {min(r[1] for r in runs):8.1f} ms  (max wall {max(r[0] for r in runs):.1f})"
                    + ("" if len(outs) == 1 else "  OUTPUT DIFFERS"))
            log(line)
            report.append(line)
    if MAC:
        exe = build(f"k{kata}_long", text.replace(iters, scaled))
        for t in (2, 4):
            report.append(f"--- kata {kata} sample, T={t}")
            report += sample(exe, t, os.path.join(OUT, f"k{kata}_T{t}.sample.txt"))
path = os.path.join(os.path.dirname(OUT), f"m5-pool2-{SHA}.txt")
with open(path, "w") as f:
    f.write("\n".join(report) + "\n")
print("\n".join(report))
print(f"\nwritten: {path}")
PY
