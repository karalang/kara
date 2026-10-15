# Python mirror of mini_shell.kara: the same tokenizer, parser and runner,
# the same script. Children are run directly with subprocess (no shell),
# pipeline stages one after another with buffered stdin, as in the Kara
# program.
import os
import subprocess

SCRIPT = r"""# quoting and expansion
echo hello   world
echo 'single $GREETING stays' "double [$GREETING]" mixed'quo'"tes"
export GREETING="hi there"
echo "$GREETING, ${GREETING}!" $GREETING
echo escaped \$GREETING and "a \"quoted\" word" back\\slash
# pipes and redirects
printf '%s\n' banana apple cherry apple | sort | uniq -c
printf 'b\na\nc\n' > list.txt
sort < list.txt
echo appended >> list.txt
cat list.txt | wc -l
cat list.txt | tr a-z A-Z | head -n 2
# status, && and ||
grep zzz list.txt && echo found || echo "not found, status $?"
false; echo "after false: $?"
true && echo and-ran; true || echo never
nosuchcmd arg; echo "status $?"
ls nosuchdir 2> err.txt; echo "ls failed: $?"; wc -l < err.txt
# the environment children see
export COUNT=3
sh -c 'echo child sees $COUNT'
unset COUNT
sh -c 'echo child sees "[$COUNT]"'
echo "${UNSET_VAR}empty" '$?' $?
# parse errors
echo unterminated 'quote
echo trailing pipe |
echo >
echo done"""


class ParseError(Exception):
    pass


def is_name_char(c):
    return c.isalnum() or c == "_"


def var_name(cs, at):
    if at >= len(cs):
        return "", at
    if cs[at] == "?":
        return "?", at + 1
    if cs[at] == "{":
        j = at + 1
        name = ""
        while j < len(cs) and cs[j] != "}":
            name += cs[j]
            j += 1
        return name, j + 1
    j = at
    name = ""
    while j < len(cs) and is_name_char(cs[j]):
        name += cs[j]
        j += 1
    return name, j


def tokenize(line):
    cs = list(line)
    toks, parts, lit = [], [], [""]
    in_word = False
    i = 0

    def flush():
        if lit[0]:
            parts.append(("lit", lit[0]))
            lit[0] = ""

    while i < len(cs):
        c = cs[i]
        if c in " \t":
            if in_word:
                flush()
                toks.append(("w", list(parts)))
                parts.clear()
                in_word = False
            i += 1
            continue
        if c == "#" and not in_word:
            break
        if c in ";|&<>" or (c == "2" and not in_word and i + 1 < len(cs) and cs[i + 1] == ">"):
            if in_word:
                flush()
                toks.append(("w", list(parts)))
                parts.clear()
                in_word = False
            two = c + cs[i + 1] if i + 1 < len(cs) else c
            op = two if two in ("&&", "||", ">>", "2>") else c
            if op == "&":
                raise ParseError("background jobs (&) are not supported")
            i += len(op)
            toks.append(("op", op))
            continue
        in_word = True
        if c == "'":
            j = i + 1
            while j < len(cs) and cs[j] != "'":
                lit[0] += cs[j]
                j += 1
            if j == len(cs):
                raise ParseError("unterminated quote")
            i = j + 1
            continue
        if c == '"':
            j = i + 1
            while j < len(cs) and cs[j] != '"':
                if cs[j] == "\\" and j + 1 < len(cs) and cs[j + 1] in '"\\$':
                    lit[0] += cs[j + 1]
                    j += 2
                    continue
                if cs[j] == "$":
                    name, nxt = var_name(cs, j + 1)
                    if not name:
                        lit[0] += "$"
                        j += 1
                    else:
                        flush()
                        parts.append(("var", name))
                        j = nxt
                    continue
                lit[0] += cs[j]
                j += 1
            if j == len(cs):
                raise ParseError("unterminated quote")
            i = j + 1
            continue
        if c == "\\":
            if i + 1 < len(cs):
                lit[0] += cs[i + 1]
            i += 2
            continue
        if c == "$":
            name, nxt = var_name(cs, i + 1)
            if not name:
                lit[0] += "$"
                i += 1
            else:
                flush()
                parts.append(("var", name))
                i = nxt
            continue
        lit[0] += c
        i += 1
    if in_word:
        flush()
        toks.append(("w", list(parts)))
    return toks


def parse(toks):
    chains, stages = [], []
    cur = {"words": [], "redirects": []}
    op = ";"
    pending = None
    for kind, v in toks:
        if kind == "w":
            if pending is not None:
                cur["redirects"].append((pending, v))
                pending = None
            else:
                cur["words"].append(v)
            continue
        o = v
        if pending is not None:
            raise ParseError(f"expected a file name before '{o}'")
        if o in ("<", ">", ">>", "2>"):
            pending = o
        elif o == "|":
            if not cur["words"]:
                raise ParseError("expected a command before '|'")
            stages.append(cur)
            cur = {"words": [], "redirects": []}
        else:
            if not cur["words"]:
                raise ParseError(f"expected a command before '{o}'")
            stages.append(cur)
            chains.append((op, stages))
            stages = []
            cur = {"words": [], "redirects": []}
            op = o
    if pending is not None:
        raise ParseError("expected a file name at end of line")
    if not cur["words"]:
        if stages:
            raise ParseError("expected a command after '|'")
        if op != ";":
            raise ParseError(f"expected a command after '{op}'")
    else:
        stages.append(cur)
    if stages:
        chains.append((op, stages))
    return chains


class Shell:
    def __init__(self):
        self.vars = {}
        self.exported = set()
        self.status = 0

    def expand(self, w):
        s = ""
        for kind, t in w:
            if kind == "lit":
                s += t
            elif t == "?":
                s += str(self.status)
            else:
                s += self.vars.get(t, "")
        return s

    def run_simple(self, cmd, inp):
        argv = [self.expand(w) for w in cmd["words"]]
        stdin_text = inp
        out_to = None
        err_to = None
        for kind, target in cmd["redirects"]:
            path = self.expand(target)
            if kind == "<":
                try:
                    with open(path) as f:
                        stdin_text = f.read()
                except OSError:
                    return f"mini_shell: {path}: No such file\n", 1
            elif kind == ">":
                out_to = (path, False)
            elif kind == ">>":
                out_to = (path, True)
            else:
                err_to = path
        out, err, code = self.exec(argv, stdin_text)
        if err_to is not None:
            with open(err_to, "w") as f:
                f.write(err)
        elif err:
            print(f"[stderr] {err}", end="")
        if out_to is not None:
            with open(out_to[0], "a" if out_to[1] else "w") as f:
                f.write(out)
            return "", code
        return out, code

    def exec(self, argv, inp):
        name = argv[0]
        if name == "echo":
            return " ".join(argv[1:]) + "\n", "", 0
        if name == "export":
            for a in argv[1:]:
                e = a.find("=")
                if e >= 0:
                    self.vars[a[:e]] = a[e + 1:]
                    self.exported.add(a[:e])
                else:
                    self.exported.add(a)
            return "", "", 0
        if name == "unset":
            for a in argv[1:]:
                self.vars.pop(a, None)
                self.exported.discard(a)
            return "", "", 0
        env = dict(os.environ)
        for k in sorted(self.exported):
            if k in self.vars:
                env[k] = self.vars[k]
        try:
            p = subprocess.run(argv, input=inp if inp is not None else None,
                               stdin=None if inp is not None else subprocess.DEVNULL,
                               capture_output=True, text=True, env=env)
        except FileNotFoundError:
            return "", f"mini_shell: {argv[0]}: command not found\n", 127
        return p.stdout, p.stderr, p.returncode

    def run_line(self, line):
        try:
            chains = parse(tokenize(line))
        except ParseError as e:
            print(f"mini_shell: parse error: {e}")
            self.status = 2
            return
        for op, stages in chains:
            if op == "&&" and self.status != 0:
                continue
            if op == "||" and self.status == 0:
                continue
            inp = None
            out, code = "", 0
            for st in stages:
                out, code = self.run_simple(st, inp)
                inp = out
            print(out, end="")
            self.status = code


def main():
    sh = Shell()
    for line in SCRIPT.splitlines():
        if not line.strip():
            continue
        if line.startswith("#"):
            print(line)
            continue
        print(f"$ {line}")
        sh.run_line(line)
        if sh.status != 0:
            print(f"[status {sh.status}]")


main()
