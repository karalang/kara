"""Mirror of mini_make.kara: a small make. Its output for the scenario in
main() is byte-identical to GNU Make 4.3 on the same files, which is the
independent check; this mirror is a port of the Kara."""
import os
import subprocess

MAKEFILE = """# "compile" upper-cases a source file, "link" concatenates objects.
CC = tr a-z A-Z
SRCS := $(wildcard src/*.c)
OBJS = $(patsubst src/%.c,build/%.o,$(SRCS))
OUT = build/app
FLAGS = -v
FLAGS += -x
LATE = $(EARLY) later
EARLY := early

.PHONY: all clean check

all: $(OUT) build/manifest.txt

$(OUT): $(OBJS) | build
\t@echo "link $@ from $(words $^) objects"
\tcat $^ > $@

build/%.o: src/%.c src/common.h | build
\t$(CC) < $< > $@

build/manifest.txt: $(OBJS)
\t@printf '%s\\n' $(notdir $^) > $@

build:
\tmkdir -p build

check: all
\t@grep -c . $(OUT)
\t-false
\t@echo "check $(FLAGS) done"

clean:
\trm -rf build

print-%:
\t@echo '$* = $($*)'
"""

BROKEN = """a: b
\techo a
b: a
\techo b
c: nothere
\techo c
d:
\texit 3
\techo unreachable
"""


class Stop(Exception):
    pass


class Rule:
    def __init__(self, targets, prereqs, order_only, line):
        self.targets, self.prereqs, self.order_only = targets, prereqs, order_only
        self.recipe = []  # (text, line)
        self.line = line


class Var:
    def __init__(self, value, recursive):
        self.value, self.recursive = value, recursive


def match_paren(s, i):
    # s[i] is the opening bracket; return the index of its partner
    open_c = s[i]
    close_c = ")" if open_c == "(" else "}"
    depth = 0
    j = i
    while j < len(s):
        if s[j] == open_c:
            depth += 1
        elif s[j] == close_c:
            depth -= 1
            if depth == 0:
                return j
        j += 1
    raise Stop("unterminated variable reference")


def split_args(s):
    out, depth, cur = [], 0, ""
    for c in s:
        if c in "({":
            depth += 1
        elif c in ")}":
            depth -= 1
        if c == "," and depth == 0:
            out.append(cur)
            cur = ""
        else:
            cur += c
    out.append(cur)
    return out


def glob_match(pat, name):
    # only '*' is supported
    parts = pat.split("*")
    if len(parts) == 1:
        return pat == name
    if not name.startswith(parts[0]) or not name.endswith(parts[-1]):
        return False
    at = len(parts[0])
    end = len(name) - len(parts[-1])
    for p in parts[1:-1]:
        k = name.find(p, at)
        if k < 0 or k + len(p) > end:
            return False
        at = k + len(p)
    return at <= end


def pattern_stem(pat, word):
    k = pat.find("%")
    pre, suf = pat[:k], pat[k + 1:]
    if len(word) >= len(pre) + len(suf) and word.startswith(pre) and word.endswith(suf):
        return word[len(pre):len(word) - len(suf)]
    return None


class Make:
    def __init__(self, path, dry):
        self.path, self.dry = path, dry
        self.vars, self.rules, self.patterns, self.phony = {}, {}, [], set()
        self.goal = None
        self.done, self.ran = {}, 0
        self.out = []

    def say(self, line):
        self.out.append(line)

    # ------------------------------------------------ expansion
    def expand(self, s, auto):
        out = ""
        i = 0
        while i < len(s):
            c = s[i]
            if c != "$" or i + 1 >= len(s):
                out += c
                i += 1
                continue
            n = s[i + 1]
            if n == "$":
                out += "$"
                i += 2
            elif n in "({":
                j = match_paren(s, i + 1)
                out += self.reference(s[i + 2:j], auto)
                i = j + 1
            else:
                out += self.lookup(n, auto)
                i += 2
        return out

    def lookup(self, name, auto):
        if name in auto:
            return auto[name]
        v = self.vars.get(name)
        if v is None:
            return ""
        return self.expand(v.value, auto) if v.recursive else v.value

    def reference(self, inner, auto):
        sp = inner.find(" ")
        if sp > 0 and inner[:sp] in ("wildcard", "patsubst", "notdir", "words"):
            fn = inner[:sp]
            args = [self.expand(a, auto) for a in split_args(inner[sp + 1:])]
            return self.function(fn, args)
        return self.lookup(self.expand(inner, auto), auto)

    def function(self, fn, args):
        if fn == "wildcard":
            found = []
            for pat in args[0].split():
                k = pat.rfind("/")
                d, base = (pat[:k], pat[k + 1:]) if k >= 0 else (".", pat)
                try:
                    names = sorted(os.listdir(d))
                except OSError:
                    names = []
                for nm in names:
                    if glob_match(base, nm):
                        found.append(nm if k < 0 else d + "/" + nm)
            return " ".join(found)
        if fn == "patsubst":
            pat, rep = args[0].strip(), args[1].strip()
            words = []
            for w in args[2].split():
                if "%" in pat:
                    stem = pattern_stem(pat, w)
                    words.append(w if stem is None else rep.replace("%", stem, 1))
                else:
                    words.append(rep if w == pat else w)
            return " ".join(words)
        if fn == "notdir":
            return " ".join(w[w.rfind("/") + 1:] for w in args[0].split())
        return str(len(args[0].split()))

    # ------------------------------------------------ parsing
    def parse(self, text):
        current = None
        for n, raw in enumerate(text.splitlines(), 1):
            if raw.startswith("\t") and current is not None:
                current.recipe.append((raw[1:], n))
                continue
            line = raw
            h = line.find("#")
            if h >= 0:
                line = line[:h]
            if not line.strip():
                continue
            a = self.assignment(line)
            if a:
                current = None
                continue
            colon = line.find(":")
            if colon < 0:
                raise Stop(f"{self.path}:{n}: *** missing separator.  Stop.")
            targets = self.expand(line[:colon], {}).split()
            rest = line[colon + 1:]
            inline = None
            semi = rest.find(";")
            if semi >= 0:
                inline = rest[semi + 1:].strip()
                rest = rest[:semi]
            bar = rest.find("|")
            normal, order = (rest[:bar], rest[bar + 1:]) if bar >= 0 else (rest, "")
            rule = Rule(targets, self.expand(normal, {}).split(), self.expand(order, {}).split(), n)
            if inline:
                rule.recipe.append((inline, n))
            current = rule
            if targets == [".PHONY"]:
                self.phony.update(rule.prereqs)
                current = None
                continue
            for t in targets:
                if "%" in t:
                    self.patterns.append((t, rule))
                else:
                    if self.goal is None and not t.startswith("."):
                        self.goal = t
                    old = self.rules.get(t)
                    if old is None:
                        self.rules[t] = rule
                    else:
                        merged = Rule([t], old.prereqs + rule.prereqs, old.order_only + rule.order_only, old.line)
                        merged.recipe = rule.recipe if rule.recipe else old.recipe
                        self.rules[t] = merged

    def assignment(self, line):
        for op in (":=", "+=", "="):
            k = line.find(op)
            if k > 0:
                name = line[:k].strip()
                if not name or not all(c.isalnum() or c == "_" for c in name):
                    return False
                if ":" in line[:k]:
                    return False
                value = line[k + len(op):].strip()
                if op == ":=":
                    self.vars[name] = Var(self.expand(value, {}), False)
                elif op == "=":
                    self.vars[name] = Var(value, True)
                else:
                    old = self.vars.get(name)
                    if old is None:
                        self.vars[name] = Var(value, True)
                    elif old.recursive:
                        old.value = (old.value + " " + value).strip()
                    else:
                        old.value = (old.value + " " + self.expand(value, {})).strip()
                return True
        return False

    # ------------------------------------------------ building
    def mtime(self, path):
        try:
            return os.stat(path).st_mtime_ns // 1_000_000
        except OSError:
            return -1

    def find_rule(self, target):
        r = self.rules.get(target)
        if r is not None and r.recipe:
            return r, None, r.prereqs, r.order_only
        for pat, rule in self.patterns:
            stem = pattern_stem(pat, target)
            if stem is None:
                continue
            pre = [p.replace("%", stem, 1) for p in rule.prereqs]
            if all(self.mtime(p) >= 0 or p in self.rules for p in pre):
                extra = r.prereqs if r else []
                eo = r.order_only if r else []
                return rule, stem, pre + extra, [p.replace("%", stem, 1) for p in rule.order_only] + eo
        if r is not None:
            return r, None, r.prereqs, r.order_only
        return None, None, [], []

    def build(self, target, parent, stack):
        if target in self.done:
            return self.done[target]
        rule, stem, prereqs, order = self.find_rule(target)
        if rule is None:
            if self.mtime(target) >= 0:
                self.done[target] = False
                return False
            if parent is None:
                raise Stop(f"*** No rule to make target '{target}'.  Stop.")
            raise Stop(f"*** No rule to make target '{target}', needed by '{parent}'.  Stop.")
        stack.append(target)
        changed = False
        kept = []
        for p in prereqs + order:
            if p in stack:
                self.say(f"make: Circular {target} <- {p} dependency dropped.")
                continue
            if self.build(p, target, stack) and p in prereqs:
                changed = True
            if p in prereqs:
                kept.append(p)
        stack.pop()
        mine = self.mtime(target)
        need = target in self.phony or mine < 0 or changed
        if not need:
            for p in kept:
                if self.mtime(p) > mine:
                    need = True
        if need and rule.recipe:
            uniq = []
            for p in kept:
                if p not in uniq:
                    uniq.append(p)
            auto = {"@": target, "<": kept[0] if kept else "", "^": " ".join(uniq), "*": stem or ""}
            for text, line in rule.recipe:
                self.run(self.expand(text, auto), target, line)
        self.done[target] = need
        return need

    def run(self, cmd, target, line):
        silent = ignore = False
        while cmd and cmd[0] in "@-+":
            if cmd[0] == "@":
                silent = True
            if cmd[0] == "-":
                ignore = True
            cmd = cmd[1:]
        cmd = cmd.strip()
        self.ran += 1
        if self.dry or not silent:
            self.say(cmd)
        if self.dry:
            return
        r = subprocess.run(["sh", "-c", cmd], capture_output=True, text=True)
        for l in (r.stdout + r.stderr).splitlines():
            self.say(l)
        if r.returncode != 0:
            if ignore:
                self.say(f"make: [{self.path}:{line}: {target}] Error {r.returncode} (ignored)")
            else:
                raise Stop(f"*** [{self.path}:{line}: {target}] Error {r.returncode}")


def make(args):
    path, dry, goals = "Makefile", False, []
    i = 0
    while i < len(args):
        if args[i] == "-f":
            path = args[i + 1]
            i += 2
        elif args[i] == "-n":
            dry = True
            i += 1
        else:
            goals.append(args[i])
            i += 1
    m = Make(path, dry)
    code = 0
    try:
        with open(path) as f:
            m.parse(f.read())
        for g in goals or [m.goal]:
            before = m.ran
            m.build(g, None, [])
            if m.ran == before:
                r = m.rules.get(g)
                has_recipe = (r is not None and r.recipe) or (r is None and any(pattern_stem(p, g) is not None for p, _ in m.patterns))
                if has_recipe and g not in m.phony:
                    m.say(f"make: '{g}' is up to date.")
                else:
                    m.say(f"make: Nothing to be done for '{g}'.")
    except Stop as e:
        m.say(f"make: {e}")
        code = 2
    print(f"== make {' '.join(args)}")
    for l in m.out:
        print(l)
    print(f"(exit {code})")


def write(path, text):
    with open(path, "w") as f:
        f.write(text)


def main():
    write("Makefile", MAKEFILE)
    write("broken.mk", BROKEN)
    os.makedirs("src", exist_ok=True)
    write("src/a.c", 'int a = 1;\n#include "common.h"\n')
    write("src/b.c", "int b = 2;\n")
    write("src/main.c", "int main(void);\n")
    write("src/common.h", "#define K 3\n")
    make([])
    make([])
    write("src/b.c", "int b = 20;\n")
    make([])
    make(["build/b.o"])
    write("src/common.h", "#define K 4\n")
    make([])
    make(["check"])
    make(["print-OBJS"])
    make(["print-LATE"])
    make(["missing"])
    make(["-f", "broken.mk", "a"])
    make(["-f", "broken.mk", "c"])
    make(["-f", "broken.mk", "d"])
    os.remove("build/a.o")
    make(["-n"])
    make(["clean"])
    make(["-n"])


main()
