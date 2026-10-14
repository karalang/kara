# Mirror of source.kara: Python's `re` (ASCII classes) is the oracle for
# matching; pattern errors are checked by a small validator that reports
# the same messages and offsets as the Kara parser for the cases used.
import os, re, shutil, subprocess

def low_pattern(p):
    out = []; i = 0
    while i < len(p):
        if p[i] == "\\" and i + 1 < len(p):
            out.append(p[i:i + 2]); i += 2
        else:
            out.append(p[i].lower()); i += 1
    return "".join(out)

def validate(p):
    depth = 0; i = 0
    while i < len(p):
        c = p[i]
        if c == "\\": i += 2; continue
        if c == "(": depth += 1
        elif c == ")":
            if depth == 0: return f"unmatched ')' at offset {i}"
            depth -= 1
        elif c == "{":
            m = re.match(r"\{(\d+)(,(\d*))?\}", p[i:])
            if m:
                lo = int(m.group(1)); hi = m.group(3)
                if m.group(2) and hi and int(hi) < lo:
                    return f"repeat range is backwards at offset {i + len(m.group(0))}"
                i += len(m.group(0)); continue
        i += 1
    if depth > 0: return f"missing ')' at offset {len(p)}"
    return None

def expand(paths):
    out = []
    for p in paths:
        if os.path.exists(p):
            if os.path.isdir(p):
                fs = []
                for d, _, names in os.walk(p):
                    fs += [os.path.join(d, n) for n in names]
                out += sorted(fs, key=lambda s: s.encode())
            else:
                out.append(p)
        else:
            out.append(p)
    return out

def grep(args):
    fold = invert = count = lst = number = only = False; ctx = 0; rest = []; i = 0
    while i < len(args):
        a = args[i]
        if a == "-C":
            i += 1; ctx = int(args[i])
        elif a.startswith("-") and len(a) > 1:
            for c in a[1:]:
                if c == "i": fold = True
                elif c == "v": invert = True
                elif c == "c": count = True
                elif c == "l": lst = True
                elif c == "n": number = True
                elif c == "o": only = True
        else:
            rest.append(a)
        i += 1
    pat = rest[0]
    e = validate(pat)
    if e:
        print(f"grep: invalid pattern: {e}"); return 2
    rx = re.compile(low_pattern(pat) if fold else pat, re.ASCII)
    prep = (lambda l: l.lower()) if fold else (lambda l: l)
    files = expand(rest[1:]); many = len(files) > 1; status = 1
    for path in files:
        try:
            text = open(path, encoding="utf-8").read()
        except OSError:
            print(f"grep: {path}: No such file or directory"); status = 2; continue
        lines = text.splitlines()
        hits = [(rx.search(prep(l)) is not None) != invert for l in lines]
        n = sum(hits)
        if n > 0 and status != 2: status = 0
        if lst:
            if n: print(path)
            continue
        if count:
            print(f"{path}:{n}" if many else f"{n}"); continue
        last = -1
        for k in range(len(lines)):
            if not hits[k]: continue
            frm = max(0, k - ctx); to = min(len(lines) - 1, k + ctx)
            if ctx > 0 and last >= 0 and frm > last + 1: print("--")
            for j in range(max(frm, last + 1), to + 1):
                sep = ":" if hits[j] else "-"
                head = (f"{path}{sep}" if many else "") + (f"{j + 1}{sep}" if number else "")
                if only and hits[j] and not invert:
                    s = prep(lines[j]); at = 0
                    while at <= len(s):
                        m = rx.search(s, at)
                        if not m: break
                        if m.end() > m.start():
                            print(f"{head}{lines[j][m.start():m.end()]}"); at = m.end()
                        else:
                            at = m.start() + 1
                elif not only:
                    print(f"{head}{lines[j]}")
            last = to
    return status

def write(p, t):
    open(p, "w", encoding="utf-8").write(t)

def main():
    shutil.rmtree("src", ignore_errors=True); shutil.rmtree("logs", ignore_errors=True)
    os.makedirs("src/net"); os.makedirs("logs")
    write("src/main.rs", "use net::Server;\n\nfn main() {\n    // TODO: read the port from ENV\n    let port = 8080;\n    let srv = Server::bind(port).expect(\"bind\");\n    srv.run();\n}\n")
    write("src/net/server.rs", "pub struct Server { port: u16 }\n\nimpl Server {\n    pub fn bind(port: u16) -> Result<Server, Error> {\n        // todo: validate port\n        Ok(Server { port })\n    }\n    pub fn run(&self) { loop {} }\n}\n")
    write("logs/app.log", "2026-10-14T10:00:01 INFO  started pid=4242\n2026-10-14T10:00:02 WARN  slow request id=17 took=1500ms\n2026-10-14T10:00:03 ERROR db: connection refused (retry 1)\n2026-10-14T10:00:04 INFO  request id=18 took=12ms\n2026-10-14T10:00:05 ERROR db: connection refused (retry 2)\n2026-10-14T10:00:06 INFO  recovered after 2 retries\n2026-10-14T10:00:07 DEBUG cache hit ratio=0.93\n2026-10-14T10:00:08 WARN  slow request id=19 took=2200ms\n")
    write("logs/notes.txt", "Café menu: crème brûlée, naïve tea\nemail: ops@example.com, dev@example.org\nphone +1 (555) 010-9999\n")
    cmds = [
        "TODO src", "-i -n todo src", "-c ERROR logs/app.log", "-v -c INFO logs/app.log",
        "-n took=[0-9]{4,}ms logs/app.log", "-o id=\\d+ logs/app.log", "-C 1 -n ERROR logs/app.log",
        "-l -i server src logs", "-o [\\w.]+@\\w+\\.(com|org) logs/notes.txt", "-o -i CR[ÈE]ME logs/notes.txt",
        "^20.*(WARN|DEBUG) logs/app.log", "-o \\(.*?\\) logs", "-c ^$ src/main.rs",
        "-o ratio=0\\.\\d? logs/app.log", "fn\\s+\\w+\\( src/net/server.rs", "nomatch logs",
        "(unclosed logs", "a{3,1} logs", "x missing.txt",
    ]
    for c in cmds:
        print(f"$ grep {c}")
        print(f"(exit {grep(c.split())})")

main()
