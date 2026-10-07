"""Python mirror of source.orig.kara (todo_store)."""

import os

STORE_PATH = "todos.db"
UNDO_PATH = "todos.undo"


class TodoError(Exception):
    pass


def not_found(i):
    return TodoError(f"no todo with id {i}")


def bad_args(msg):
    return TodoError(f"bad arguments: {msg}")


def join_from(parts, start, sep):
    return sep.join(parts[start:])


def parse_id(word):
    try:
        if word.strip() != word or word == "":
            raise ValueError
        return int(word)
    except ValueError:
        raise bad_args(f"'{word}' is not a valid id")


class Todo:
    def __init__(self, id_, title):
        self.id = id_
        self.title = title
        self.done = False
        self.tags = []

    def has_tag(self, tag):
        return tag in self.tags

    def to_line(self):
        flag = "1" if self.done else "0"
        tags = "-" if not self.tags else ",".join(self.tags)
        return f"item|{self.id}|{flag}|{tags}|{self.title}"

    def render(self):
        mark = "x" if self.done else " "
        out = f"  [{mark}] #{self.id} {self.title}"
        if self.tags:
            out += f"  ({', '.join(self.tags)})"
        return out


class Filter:
    def __init__(self, kind, tag=None):
        self.kind = kind
        self.tag = tag

    @staticmethod
    def parse(word):
        if word == "" or word == "all":
            return Filter("all")
        if word == "open":
            return Filter("open")
        if word == "done":
            return Filter("done")
        if not word.startswith("tag:") or len(word) == 4:
            raise bad_args(f"unknown list filter '{word}'")
        return Filter("tag", word[4:])

    def matches(self, t):
        if self.kind == "all":
            return True
        if self.kind == "open":
            return not t.done
        if self.kind == "done":
            return t.done
        return t.has_tag(self.tag)

    def label(self):
        if self.kind == "tag":
            return f"tag {self.tag}"
        return self.kind


def need_id(words, op):
    if len(words) < 2:
        raise bad_args(f"{op} needs an id")
    return parse_id(words[1])


def need_text(words, start, complaint):
    text = join_from(words, start, " ")
    if text == "":
        raise bad_args(complaint)
    return text


def parse_command(line):
    words = line.strip().split(" ")
    op = words[0]
    if op == "add":
        return ("add", need_text(words, 1, "add needs a title"))
    if op == "list":
        return ("list", Filter.parse(join_from(words, 1, " ")))
    if op == "undo":
        return ("undo",)
    if op == "done":
        return ("done", need_id(words, op))
    if op == "remove":
        return ("remove", need_id(words, op))
    if op == "edit":
        id_ = need_id(words, op)
        return ("edit", id_, need_text(words, 2, "edit needs a new title"))
    if op == "tag":
        id_ = need_id(words, op)
        tag = need_text(words, 2, "tag needs a tag name")
        if "," in tag or "|" in tag or " " in tag:
            raise bad_args(f"tag '{tag}' may not contain ',', '|' or spaces")
        return ("tag", id_, tag)
    raise TodoError(f"unknown command '{op}'")


class Store:
    def __init__(self):
        self.next_id = 1
        self.items = []

    def serialize(self):
        out = f"next|{self.next_id}\n"
        for t in self.items:
            out += t.to_line() + "\n"
        return out

    @staticmethod
    def parse(text):
        store = Store()
        lineno = 0
        for line in text.split("\n"):
            lineno += 1
            if line == "":
                continue
            parts = line.split("|")
            if parts[0] == "next" and len(parts) == 2:
                store.next_id = parse_id(parts[1])
            elif parts[0] == "item" and len(parts) >= 5:
                todo = Todo(parse_id(parts[1]), join_from(parts, 4, "|"))
                todo.done = parts[2] == "1"
                if parts[3] != "-":
                    todo.tags = parts[3].split(",")
                store.items.append(todo)
            else:
                raise TodoError(f"corrupt store at line {lineno}: {line}")
        return store

    def index_of(self, id_):
        for i, t in enumerate(self.items):
            if t.id == id_:
                return i
        raise not_found(id_)

    def list(self, flt):
        out = ""
        shown = 0
        for t in self.items:
            if flt.matches(t):
                out += t.render() + "\n"
                shown += 1
        if shown == 0:
            out += "  (nothing)\n"
        out += f"  -- {shown} shown ({flt.label()}) of {len(self.items)} --"
        return out

    def apply(self, cmd):
        kind = cmd[0]
        if kind == "add":
            id_ = self.next_id
            self.next_id += 1
            self.items.append(Todo(id_, cmd[1]))
            return f"added #{id_}: {cmd[1]}"
        if kind == "done":
            idx = self.index_of(cmd[1])
            if self.items[idx].done:
                raise TodoError(f"todo #{cmd[1]} is already done")
            self.items[idx].done = True
            return f"completed #{cmd[1]}: {self.items[idx].title}"
        if kind == "edit":
            idx = self.index_of(cmd[1])
            msg = f"renamed #{cmd[1]}: '{self.items[idx].title}' -> '{cmd[2]}'"
            self.items[idx].title = cmd[2]
            return msg
        if kind == "tag":
            idx = self.index_of(cmd[1])
            if self.items[idx].has_tag(cmd[2]):
                raise TodoError(f"todo #{cmd[1]} is already tagged '{cmd[2]}'")
            self.items[idx].tags.append(cmd[2])
            return f"tagged #{cmd[1]} with '{cmd[2]}'"
        if kind == "list":
            return self.list(cmd[1])
        if kind == "remove":
            idx = self.index_of(cmd[1])
            gone = self.items.pop(idx)
            return f"removed #{cmd[1]}: {gone.title}"
        raise bad_args("undo is handled by the session")


def read_or_empty(path):
    if not os.path.exists(path):
        return ""
    with open(path) as f:
        return f.read()


def write(path, text):
    with open(path, "w") as f:
        f.write(text)


def load_store():
    return Store.parse(read_or_empty(STORE_PATH))


def undo_last():
    snapshot = read_or_empty(UNDO_PATH)
    if snapshot == "":
        raise TodoError("nothing to undo")
    restored = Store.parse(snapshot)
    write(STORE_PATH, restored.serialize())
    write(UNDO_PATH, "")
    return f"undid last change; {len(restored.items)} todo(s) restored"


def run_line(line):
    cmd = parse_command(line)
    if cmd[0] == "undo":
        return undo_last()
    store = load_store()
    before = store.serialize()
    mutating = cmd[0] != "list"
    msg = store.apply(cmd)
    if mutating:
        write(UNDO_PATH, before)
    write(STORE_PATH, store.serialize())
    return msg


def summary(store):
    done = 0
    tag_counts = {}
    for t in store.items:
        if t.done:
            done += 1
        for tag in t.tags:
            tag_counts[tag] = tag_counts.get(tag, 0) + 1
    total = len(store.items)
    out = f"total {total}, done {done}, open {total - done}"
    for tag in sorted(tag_counts):
        out += f"\n  tag {tag}: {tag_counts[tag]}"
    return out


def main():
    script = [
        "add Buy milk",
        "add Write quarterly report",
        "add Call the plumber",
        "tag 2 work",
        "tag 2 urgent",
        "tag 3 home",
        "done 1",
        "list",
        "edit 3 Call the plumber about the leak",
        "tag 2 work",
        "done 1",
        "done 42",
        "remove 1",
        "list all",
        "undo",
        "list",
        "undo",
        "add Fix bug | high prio",
        "tag 4 work",
        "done 2",
        "list open",
        "list done",
        "list tag:work",
        "list someday",
        "frobnicate 3",
        "edit 4",
        "tag 4 bad,tag",
        "remove x",
        "remove 3",
        "undo",
        "tag 1 home",
        "list",
    ]
    write(STORE_PATH, Store().serialize())
    write(UNDO_PATH, "")
    ok = 0
    failed = 0
    for line in script:
        print(f"> {line}")
        try:
            print(run_line(line))
            ok += 1
        except TodoError as e:
            print(f"error: {e}")
            failed += 1
    print(f"== {ok} ok, {failed} failed ==")
    print(summary(load_store()))
    print("== final dump ==")
    print(read_or_empty(STORE_PATH), end="")


if __name__ == "__main__":
    main()
