# Python mirror of source.kara: same task file, same processes, same output.
import json
import os
import subprocess

TASKS = """# name | deps | command | stdin
gen     |            | printf 'pear\\napple\\nfig\\napple\\nkiwi\\n'
sort    | gen        | sort | @gen
uniq    | sort       | uniq -c | @sort
count   | sort       | wc -l | @sort
greet   |            | echo "hello $WHO from $STAGE" | | WHO=runner STAGE=build
fail    | gen        | echo broken >&2; echo half; exit 3
after   | fail       | echo never runs
deeper  | after uniq | echo never either
report  | uniq count | echo report ready for $WHO | | WHO=ci
"""

def parse_tasks(text):
    tasks = []
    for line_no, line in enumerate(text.splitlines(), 1):
        t = line.strip()
        if not t or t.startswith("#"):
            continue
        parts = [p.strip() for p in t.split("|")]
        if len(parts) < 3:
            raise ValueError(f"line {line_no}: want name | deps | command")
        inp = parts[3][1:] if len(parts) > 3 and parts[3].startswith("@") else None
        env = []
        if len(parts) > 4:
            for kv in parts[4].split():
                k, _, v = kv.partition("=")
                env.append((k, v))
        tasks.append({"name": parts[0], "deps": parts[1].split(), "cmd": parts[2], "input": inp, "env": env})
    return tasks

def order(tasks):
    n = len(tasks)
    names = [t["name"] for t in tasks]
    indeg = [0] * n
    users = [[] for _ in range(n)]
    for i, t in enumerate(tasks):
        for d in t["deps"]:
            j = names.index(d)
            indeg[i] += 1
            users[j].append(i)
    out, done = [], [False] * n
    while len(out) < n:
        pick = next(k for k in range(n) if not done[k] and indeg[k] == 0)
        done[pick] = True
        out.append(pick)
        for u in users[pick]:
            indeg[u] -= 1
    return out

def count_lines(s):
    return sum(1 for l in s.splitlines() if l.strip())

def run_task(t, inp):
    env = dict(os.environ)
    env.update(dict(t["env"]))
    p = subprocess.run(["sh", "-c", t["cmd"]], input=inp if inp is not None else None,
                       stdin=None if inp is not None else subprocess.DEVNULL,
                       capture_output=True, text=True, env=env)
    return {"name": t["name"], "status": "ok" if p.returncode == 0 else "failed",
            "code": p.returncode, "stdout": p.stdout, "stderr_lines": count_lines(p.stderr)}

def main():
    with open("tasks.txt", "w") as f:
        f.write(TASKS)
    tasks = parse_tasks(open("tasks.txt").read())
    print(f"{len(tasks)} tasks")
    plan = order(tasks)
    print("order: " + " ".join(tasks[i]["name"] for i in plan))
    outputs, failed, results = {}, set(), []
    for i in plan:
        t = tasks[i]
        blocked = next((d for d in t["deps"] if d in failed), None)
        if blocked is not None:
            print(f"[skip] {t['name']} (needs {blocked})")
            failed.add(t["name"])
            results.append({"name": t["name"], "status": "skipped", "code": -1, "stdout": "", "stderr_lines": 0})
            continue
        inp = outputs.get(t["input"]) if t["input"] is not None else None
        r = run_task(t, inp)
        lines = r["stdout"].splitlines()
        first = lines[0].strip() if lines else ""
        print(f"[{r['status']}] {t['name']} exit={r['code']} lines={count_lines(r['stdout'])} "
              f"stderr={r['stderr_lines']} first=[{first}]")
        if r["code"] != 0:
            failed.add(t["name"])
        outputs[t["name"]] = r["stdout"]
        results.append(r)
    ok = sum(1 for r in results if r["status"] == "ok")
    bad = sum(1 for r in results if r["status"] == "failed")
    skipped = len(results) - ok - bad
    with open("report.json", "w") as f:
        f.write(json.dumps({"ok": ok, "failed": bad, "skipped": skipped, "results": results}, separators=(",", ":")))
    r = json.loads(open("report.json").read())
    print(f"report: ok={r['ok']} failed={r['failed']} skipped={r['skipped']}")
    for t in r["results"]:
        if t["name"] == "uniq":
            for l in t["stdout"].splitlines():
                print(f"  uniq: {l.strip()}")
    try:
        open("missing.json").read()
        print("missing.json exists?")
    except FileNotFoundError:
        print("missing.json: not found")

main()
