"""Mirror of supervisor.kara: a process supervisor in the style of
supervisord. It is a port of the Kara; the command splitter is also checked
against Python's shlex here."""
import os
import shlex
import subprocess
import time

CONFIG = """; supervisor.ini: the programs to keep running
[supervisor]
poll_ms = 10

[program:flaky]
; fails twice, then comes up
command = sh -c 'n=$(cat flaky.count 2>/dev/null || echo 0); n=$((n+1)); echo $n > flaky.count; echo "flaky attempt $n"; [ $n -ge 3 ] && exit 0; exit 1'
autorestart = unexpected
startretries = 3
backoff_ms = 30

[program:broken]
command = sh -c "echo 'cannot bind port' >&2; exit 2"
autorestart = true
startretries = 2
backoff_ms = 20

[program:hang]
command = sleep 30
max_runtime_ms = 300
autorestart = false

[program:chatty]
command = sh -c 'for i in 1 2 3 4 5 6 7; do echo "line $i of the chatty log"; done; echo oops >&2'
stdout_logfile = chatty.log
stdout_maxbytes = 60
stdout_backups = 2
colour = blue

[program:greeter]
command = sh -c 'echo "$GREETING from $SUPERVISOR_PROGRAM (level $LEVEL)"'
environment = GREETING="hello there",LEVEL=3

[program:ghost]
command = /nonexistent/daemon --serve
startretries = 1

[program:graceful]
command = sh -c "echo draining; exit 3"
exitcodes = 0,3
autorestart = unexpected

[program:nocmd]
autorestart = true

[program:typo]
command = true
startretries = lots
"""


class Prog:
    def __init__(self, name):
        self.name = name
        self.command = None
        self.argv = []
        self.autorestart = "unexpected"
        self.startretries = 3
        self.backoff_ms = 50
        self.max_runtime_ms = 0
        self.exitcodes = [0]
        self.env = []
        self.logfile = ""
        self.maxbytes = 0
        self.backups = 0
        self.state = "STOPPED"
        self.starts = 0
        self.last = "-"
        self.events = []
        self.proc = None
        self.started_at = 0
        self.next_start = 0


def split_command(s):
    words, cur, have, i = [], "", False, 0
    while i < len(s):
        c = s[i]
        if c in " \t":
            if have:
                words.append(cur)
                cur, have = "", False
            i += 1
        elif c == "'":
            j = s.find("'", i + 1)
            if j < 0:
                raise ValueError("unterminated single quote")
            cur += s[i + 1:j]
            have = True
            i = j + 1
        elif c == '"':
            i += 1
            while True:
                if i >= len(s):
                    raise ValueError("unterminated double quote")
                if s[i] == '"':
                    break
                if s[i] == "\\" and i + 1 < len(s) and s[i + 1] in '"\\$`':
                    cur += s[i + 1]
                    i += 2
                else:
                    cur += s[i]
                    i += 1
            have = True
            i += 1
        elif c == "\\" and i + 1 < len(s):
            cur += s[i + 1]
            have = True
            i += 2
        else:
            cur += c
            have = True
            i += 1
    if have:
        words.append(cur)
    return words


def parse_env(s):
    out, i = [], 0
    while i < len(s):
        eq = s.find("=", i)
        if eq < 0:
            raise ValueError(f"no '=' in environment entry '{s[i:]}'")
        key = s[i:eq].strip()
        i = eq + 1
        if i < len(s) and s[i] == '"':
            j = s.find('"', i + 1)
            if j < 0:
                raise ValueError("unterminated quote in environment")
            val = s[i + 1:j]
            i = j + 1
        else:
            j = s.find(",", i)
            j = len(s) if j < 0 else j
            val = s[i:j].strip()
            i = j
        out.append((key, val))
        if i < len(s) and s[i] == ",":
            i += 1
    return out


def parse_int(v):
    try:
        return int(v)
    except ValueError:
        return None


def parse_config(text):
    progs, warnings, errors = [], [], []
    poll = 10
    cur, section = None, ""
    for n, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith(";") or line.startswith("#"):
            continue
        k = line.find(" ;")
        if k >= 0:
            line = line[:k].rstrip()
        if line.startswith("["):
            if not line.endswith("]"):
                errors.append(f"line {n}: bad section header")
                continue
            section = line[1:-1]
            cur = None
            if section.startswith("program:"):
                cur = Prog(section[8:])
                progs.append(cur)
            elif section != "supervisor":
                warnings.append(f"line {n}: unknown section [{section}]")
            continue
        eq = line.find("=")
        if eq < 0:
            errors.append(f"line {n}: expected key = value")
            continue
        key, val = line[:eq].strip(), line[eq + 1:].strip()
        if section == "supervisor":
            if key == "poll_ms" and parse_int(val) is not None:
                poll = parse_int(val)
            else:
                warnings.append(f"line {n}: unknown key '{key}' in [supervisor]")
            continue
        if cur is None:
            warnings.append(f"line {n}: '{key}' outside a section")
            continue
        p = cur
        if key == "command":
            p.command = val
            try:
                p.argv = split_command(val)
            except ValueError as e:
                errors.append(f"line {n} [{p.name}]: {e}")
        elif key == "autorestart":
            if val in ("true", "false", "unexpected"):
                p.autorestart = val
            else:
                errors.append(f"line {n} [{p.name}]: autorestart must be true, false or unexpected")
        elif key in ("startretries", "backoff_ms", "max_runtime_ms", "stdout_maxbytes", "stdout_backups"):
            v = parse_int(val)
            if v is None or v < 0:
                errors.append(f"line {n} [{p.name}]: {key} is not a count: '{val}'")
            elif key == "startretries":
                p.startretries = v
            elif key == "backoff_ms":
                p.backoff_ms = v
            elif key == "max_runtime_ms":
                p.max_runtime_ms = v
            elif key == "stdout_maxbytes":
                p.maxbytes = v
            else:
                p.backups = v
        elif key == "exitcodes":
            codes = [parse_int(x.strip()) for x in val.split(",")]
            if None in codes:
                errors.append(f"line {n} [{p.name}]: bad exitcodes '{val}'")
            else:
                p.exitcodes = codes
        elif key == "environment":
            try:
                p.env = parse_env(val)
            except ValueError as e:
                errors.append(f"line {n} [{p.name}]: {e}")
        elif key == "stdout_logfile":
            p.logfile = val
        else:
            warnings.append(f"line {n}: unknown key '{key}' in [program:{p.name}]")
    kept = []
    for p in progs:
        if p.command is None:
            errors.append(f"[{p.name}]: no command, program skipped")
        else:
            kept.append(p)
    return kept, poll, warnings, errors


def rotate(p, data):
    """Append data to p.logfile, rotating to .1, .2 ... when it would grow
    past maxbytes. Returns the number of rotations."""
    rotations = 0
    for line in data.splitlines(keepends=True):
        size = os.path.getsize(p.logfile) if os.path.exists(p.logfile) else 0
        if p.maxbytes > 0 and size > 0 and size + len(line) > p.maxbytes:
            for k in range(p.backups, 0, -1):
                src = p.logfile if k == 1 else f"{p.logfile}.{k - 1}"
                dst = f"{p.logfile}.{k}"
                if os.path.exists(src):
                    if os.path.exists(dst):
                        os.remove(dst)
                    os.rename(src, dst)
            if p.backups == 0 and os.path.exists(p.logfile):
                os.remove(p.logfile)
            rotations += 1
        with open(p.logfile, "a") as f:
            f.write(line)
    return rotations


def start(p, now):
    p.starts += 1
    env = dict(os.environ)
    for k, v in p.env:
        env[k] = v
    env["SUPERVISOR_PROGRAM"] = p.name
    try:
        p.proc = subprocess.Popen(p.argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True)
    except FileNotFoundError:
        p.proc = None
        p.events.append(f"start {p.starts}: spawn failed: not found")
        return False
    p.state = "RUNNING"
    p.started_at = now
    p.events.append(f"start {p.starts}: running")
    return True


def give_up_or_retry(p, now, why):
    retries_used = p.starts - 1
    if retries_used < p.startretries:
        wait = p.backoff_ms * (retries_used + 1)
        p.state = "BACKOFF"
        p.next_start = now + wait
        p.events.append(f"  {why}; retry {retries_used + 1} of {p.startretries} in {wait} ms")
    else:
        p.state = "FATAL"
        p.events.append(f"  {why}; gave up after {p.starts} start(s)")


def finished(p, code, now):
    out, err = p.proc.stdout.read(), p.proc.stderr.read()
    p.proc.stdout.close()
    p.proc.stderr.close()
    p.proc = None
    for l in out.splitlines():
        if not p.logfile:
            p.events.append(f"  | {l}")
    if p.logfile and out:
        r = rotate(p, out)
        p.events.append(f"  wrote {len(out)} bytes to {p.logfile}, {r} rotation(s)")
    for l in err.splitlines():
        p.events.append(f"  ! {l}")
    p.last = str(code)
    expected = code in p.exitcodes
    if p.autorestart == "true" or (p.autorestart == "unexpected" and not expected):
        give_up_or_retry(p, now, f"exited {code} ({'expected' if expected else 'unexpected'})")
    else:
        p.state = "EXITED"
        p.events.append(f"  exited {code} ({'expected' if expected else 'unexpected'})")


def supervise(progs, poll):
    now = 0
    for p in progs:
        if not start(p, now):
            give_up_or_retry(p, now, "spawn failed")
    while any(p.state in ("RUNNING", "BACKOFF") for p in progs):
        time.sleep(poll / 1000)
        now += poll
        for p in progs:
            if p.state == "RUNNING":
                code = p.proc.poll()
                if code is not None:
                    finished(p, code, now)
                elif p.max_runtime_ms > 0 and now - p.started_at >= p.max_runtime_ms:
                    p.proc.kill()
                    p.proc.wait()
                    p.proc.stdout.close()
                    p.proc.stderr.close()
                    p.proc = None
                    p.state = "KILLED"
                    p.last = "killed"
                    p.events.append(f"  ran past {p.max_runtime_ms} ms; killed")
            elif p.state == "BACKOFF" and now >= p.next_start:
                if not start(p, now):
                    give_up_or_retry(p, now, "spawn failed")


def main():
    with open("supervisor.ini", "w") as f:
        f.write(CONFIG)
    with open("supervisor.ini") as f:
        progs, poll, warnings, errors = parse_config(f.read())
    print(f"config: {len(progs)} programs, poll every {poll} ms, {len(warnings)} warning(s), {len(errors)} error(s)")
    for w in warnings:
        print(f"  warning: {w}")
    for e in errors:
        print(f"  error: {e}")
    for p in progs:
        assert p.argv == shlex.split(p.command), p.name
        print(f"  {p.name}: {len(p.argv)} words, argv[0]={p.argv[0]}, last=[{p.argv[-1]}]")
    print("parse checks:")
    for s in ['a "b c" d', "it's", "x\\ y 'z w'", 'say "q\\"uote"']:
        try:
            print(f"  [{s}] -> {split_command(s)}")
        except ValueError as e:
            print(f"  [{s}] -> error: {e}")
    for s in ['A=1,B="x, y",C= z ', "NOEQ"]:
        try:
            print(f"  env [{s}] -> {parse_env(s)}")
        except ValueError as e:
            print(f"  env [{s}] -> error: {e}")

    supervise(progs, poll)
    print("== events")
    for p in progs:
        print(f"[{p.name}]")
        for e in p.events:
            print(f"  {e}")
    print("== status")
    print(f"  {'name':<9} {'state':<8} {'starts':>6}  last")
    for p in progs:
        print(f"  {p.name:<9} {p.state:<8} {p.starts:>6}  {p.last}")
    print("== chatty logs")
    for name in ["chatty.log", "chatty.log.1", "chatty.log.2", "chatty.log.3"]:
        if os.path.exists(name):
            with open(name) as f:
                lines = f.read().splitlines()
            print(f"  {name}: {len(lines)} lines, first [{lines[0]}]")
        else:
            print(f"  {name}: absent")
    with open("flaky.count") as f:
        print(f"flaky.count = {f.read().strip()}")


main()
