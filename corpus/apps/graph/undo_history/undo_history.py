"""Python mirror of source.orig.kara: editor buffer with grouped undo/redo."""


class Command:
    def __init__(self, kind, pos, length=0, text=""):
        self.kind = kind
        self.pos = pos
        self.length = length
        self.text = text

    def describe(self):
        if self.kind == "insert":
            return f'insert@{self.pos} "{self.text}"'
        if self.kind == "delete":
            return f"delete@{self.pos}+{self.length}"
        return f'replace@{self.pos}+{self.length} "{self.text}"'


class EditError(Exception):
    pass


class Edit:
    def __init__(self, pos, removed, inserted):
        self.pos = pos
        self.removed = removed
        self.inserted = inserted


class Group:
    def __init__(self, label, edits):
        self.label = label
        self.edits = edits


class Buffer:
    def __init__(self):
        self.text = ""

    def splice(self, pos, length, insert):
        size = len(self.text)
        if pos < 0 or length < 0 or pos + length > size:
            raise EditError(f"range {pos}..{pos + length} out of bounds for buffer of size {size}")
        removed = self.text[pos:pos + length]
        self.text = self.text[:pos] + insert + self.text[pos + length:]
        return removed


class Editor:
    def __init__(self):
        self.buffer = Buffer()
        self.undo_stack = []
        self.redo_stack = []
        self.group_label = None
        self.pending = []
        self.applied = 0
        self.undone = 0
        self.redone = 0
        self.discarded = 0

    def execute(self, cmd):
        if cmd.kind == "insert":
            removed = self.buffer.splice(cmd.pos, 0, cmd.text)
            inserted = cmd.text
        elif cmd.kind == "delete":
            removed = self.buffer.splice(cmd.pos, cmd.length, "")
            inserted = ""
        else:
            removed = self.buffer.splice(cmd.pos, cmd.length, cmd.text)
            inserted = cmd.text
        edit = Edit(cmd.pos, removed, inserted)
        self.applied += 1
        dropped = len(self.redo_stack)
        self.redo_stack.clear()
        self.discarded += dropped
        if self.group_label is not None:
            self.pending.append(edit)
        else:
            self.undo_stack.append(Group(cmd.describe(), [edit]))
        return dropped

    def begin(self, label):
        if self.group_label is not None:
            raise EditError(f'group "{self.group_label}" is already open')
        self.group_label = label

    def end(self):
        if self.group_label is None:
            raise EditError("no group is open")
        label = self.group_label
        self.group_label = None
        if not self.pending:
            raise EditError(f'group "{label}" recorded no edits')
        edits = self.pending
        self.pending = []
        self.undo_stack.append(Group(label, edits))
        return len(edits)

    def check_no_group(self):
        if self.group_label is not None:
            raise EditError(f'cannot undo/redo while group "{self.group_label}" is open')

    def undo(self):
        self.check_no_group()
        if not self.undo_stack:
            raise EditError("nothing to undo")
        batch = self.undo_stack.pop()
        for e in reversed(batch.edits):
            self.buffer.splice(e.pos, len(e.inserted), e.removed)
        self.undone += 1
        self.redo_stack.append(batch)
        return batch.label

    def redo(self):
        self.check_no_group()
        if not self.redo_stack:
            raise EditError("nothing to redo")
        batch = self.redo_stack.pop()
        for e in batch.edits:
            self.buffer.splice(e.pos, len(e.removed), e.inserted)
        self.redone += 1
        self.undo_stack.append(batch)
        return batch.label

    def run_step(self, step):
        kind = step[0]
        if kind == "do":
            cmd = step[1]
            desc = cmd.describe()
            dropped = self.execute(cmd)
            if dropped > 0:
                return f"{desc} (discarded {dropped} redo)"
            return desc
        if kind == "undo":
            return f"undo [{self.undo()}]"
        if kind == "redo":
            return f"redo [{self.redo()}]"
        if kind == "begin":
            msg = f'begin group "{step[1]}"'
            self.begin(step[1])
            return msg
        count = self.end()
        return f"end group ({count} edits)"


def labels(groups):
    out = " | ".join(g.label for g in reversed(groups))
    return out if out else "(empty)"


def ins(pos, text):
    return ("do", Command("insert", pos, 0, text))


def dele(pos, length):
    return ("do", Command("delete", pos, length))


def rep(pos, length, text):
    return ("do", Command("replace", pos, length, text))


UNDO = ("undo",)
REDO = ("redo",)
END = ("end",)


def begin(name):
    return ("begin", name)


def script():
    return [
        ins(0, "Hello world"),
        ins(5, ","),
        rep(7, 5, "Kara"),
        begin("decorate"),
        ins(11, "!"),
        ins(0, ">> "),
        END,
        UNDO,
        UNDO,
        REDO,
        dele(0, 7),
        REDO,
        dele(10, 3),
        ins(4, " rocks"),
        begin("shout"),
        rep(0, 4, "KARA"),
        UNDO,
        begin("again"),
        rep(5, 5, "ROCKS"),
        END,
        END,
        begin("nothing"),
        END,
        UNDO, UNDO, UNDO, UNDO, UNDO, UNDO, UNDO,
        REDO, REDO, REDO,
        rep(0, 5, "Howdy"),
        REDO,
        UNDO,
        REDO,
    ]


def main():
    editor = Editor()
    n = 0
    failures = 0
    for step in script():
        n += 1
        tag = f"0{n}" if n < 10 else f"{n}"
        try:
            msg = editor.run_step(step)
            print(f"[{tag}] {msg}")
        except EditError as e:
            failures += 1
            print(f"[{tag}] error: {e}")
        print(f"     |{editor.buffer.text}|")
    print("--- summary ---")
    text = editor.buffer.text
    print(f'final buffer: "{text}" ({len(text)} bytes)')
    print(f"undo stack: {labels(editor.undo_stack)}")
    print(f"redo stack: {labels(editor.redo_stack)}")
    print(f"applied={editor.applied} undone={editor.undone} redone={editor.redone} "
          f"discarded={editor.discarded} errors={failures}")


if __name__ == "__main__":
    main()
