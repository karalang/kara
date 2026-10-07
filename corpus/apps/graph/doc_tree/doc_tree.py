"""Python mirror of source.orig.kara: document tree with parent links."""

DOCUMENT, SECTION, PARAGRAPH = "Document", "Section", "Paragraph"


class TreeError(Exception):
    pass


class Node:
    def __init__(self, kind, title, parent):
        self.kind = kind
        self.title = title
        self.parent = parent
        self.children = []
        self.alive = True


def indent(depth):
    return "  " * depth


class DocTree:
    def __init__(self, title):
        self.nodes = [Node(DOCUMENT, title, None)]
        self.root = 0

    def check(self, nid):
        if nid < 0 or nid >= len(self.nodes) or not self.nodes[nid].alive:
            raise TreeError(f"no such node #{nid}")

    def append(self, parent, kind, title):
        self.check(parent)
        return self.insert_at(parent, len(self.nodes[parent].children), kind, title)

    def insert_at(self, parent, index, kind, title):
        self.check(parent)
        if self.nodes[parent].kind == PARAGRAPH:
            raise TreeError(f"'{self.path_of(parent)}' is a paragraph and cannot have children")
        n = len(self.nodes[parent].children)
        if index < 0 or index > n:
            raise TreeError(f"index {index} out of range (0..={n})")
        nid = len(self.nodes)
        self.nodes.append(Node(kind, title, parent))
        self.nodes[parent].children.insert(index, nid)
        return nid

    def position_in_parent(self, nid):
        p = self.nodes[nid].parent
        if p is None:
            return -1
        sib = self.nodes[p].children
        for i in range(len(sib)):
            if sib[i] == nid:
                return i
        return -1

    def is_ancestor_or_self(self, anc, node):
        cur = node
        while cur is not None:
            if cur == anc:
                return True
            cur = self.nodes[cur].parent
        return False

    def depth(self, nid):
        d = 0
        cur = self.nodes[nid].parent
        while cur is not None:
            d += 1
            cur = self.nodes[cur].parent
        return d

    def path_of(self, nid):
        parts = []
        cur = nid
        while cur != self.root:
            parts.append(self.nodes[cur].title)
            p = self.nodes[cur].parent
            if p is None:
                break
            cur = p
        parts.reverse()
        return "/".join(parts)

    def rank_among_kind(self, nid):
        rank = 0
        p = self.nodes[nid].parent
        if p is None:
            return 0
        for c in self.nodes[p].children:
            if self.nodes[c].kind == self.nodes[nid].kind:
                rank += 1
            if c == nid:
                return rank
        return rank

    def number_of(self, nid):
        parts = []
        cur = nid
        while cur != self.root:
            rank = self.rank_among_kind(cur)
            if self.nodes[cur].kind == PARAGRAPH:
                parts.append(f"p{rank}")
            else:
                parts.append(f"{rank}")
            p = self.nodes[cur].parent
            if p is None:
                break
            cur = p
        parts.reverse()
        return ".".join(parts)

    def find_by_path(self, path):
        cur = self.root
        for seg in path.split("/"):
            nxt = None
            for c in self.nodes[cur].children:
                if self.nodes[c].title == seg:
                    nxt = c
                    break
            if nxt is None:
                raise TreeError(f"path not found: '{path}'")
            cur = nxt
        return cur

    def detach(self, nid):
        pos = self.position_in_parent(nid)
        p = self.nodes[nid].parent
        if p is not None:
            self.nodes[p].children.pop(pos)
        self.nodes[nid].parent = None

    def move_subtree(self, nid, new_parent, index):
        self.check(nid)
        self.check(new_parent)
        if nid == self.root:
            raise TreeError("the document root cannot be moved or deleted")
        if self.nodes[new_parent].kind == PARAGRAPH:
            raise TreeError(f"'{self.path_of(new_parent)}' is a paragraph and cannot have children")
        if self.is_ancestor_or_self(nid, new_parent):
            raise TreeError(
                f"cannot move '{self.path_of(nid)}' under its own descendant '{self.path_of(new_parent)}'"
            )
        n = len(self.nodes[new_parent].children)
        if self.nodes[nid].parent == new_parent:
            n -= 1
        if index < 0 or index > n:
            raise TreeError(f"index {index} out of range (0..={n})")
        self.detach(nid)
        self.nodes[new_parent].children.insert(index, nid)
        self.nodes[nid].parent = new_parent

    def delete(self, nid):
        self.check(nid)
        if nid == self.root:
            raise TreeError("the document root cannot be moved or deleted")
        self.detach(nid)
        stack = [nid]
        removed = 0
        while stack:
            n = stack.pop()
            self.nodes[n].alive = False
            removed += 1
            for c in self.nodes[n].children:
                stack.append(c)
            self.nodes[n].children = []
        return removed

    def render_into(self, nid, depth, out):
        pad = indent(depth)
        node = self.nodes[nid]
        if node.kind == DOCUMENT:
            out.append(f"# {node.title}")
        elif node.kind == SECTION:
            out.append(f"{pad}{self.number_of(nid)} {node.title}")
        else:
            out.append(f"{pad}- {node.title}")
        for c in node.children:
            self.render_into(c, depth + 1, out)

    def render(self):
        lines = []
        self.render_into(self.root, 0, lines)
        return lines

    def stats(self):
        sections = paragraphs = max_depth = 0
        for i in range(len(self.nodes)):
            if not self.nodes[i].alive:
                continue
            k = self.nodes[i].kind
            if k == SECTION:
                sections += 1
            elif k == PARAGRAPH:
                paragraphs += 1
            d = self.depth(i)
            if d > max_depth:
                max_depth = d
        return f"sections={sections} paragraphs={paragraphs} max_depth={max_depth}"


def print_outline(doc, heading):
    print(f"== {heading} ==")
    for line in doc.render():
        print(line)
    print(doc.stats())


def report(label, fn):
    try:
        fn()
        print(f"[ok]   {label}")
    except TreeError as e:
        print(f"[fail] {label}: {e}")


def locate(doc, path):
    try:
        nid = doc.find_by_path(path)
        print(f"find '{path}' -> #{nid} number={doc.number_of(nid)} depth={doc.depth(nid)} path={doc.path_of(nid)}")
    except TreeError as e:
        print(f"find '{path}' -> error: {e}")


def build_handbook():
    doc = DocTree("Kara Handbook")
    root = doc.root
    intro = doc.append(root, SECTION, "Introduction")
    doc.append(intro, PARAGRAPH, "Why another systems language")
    doc.append(intro, PARAGRAPH, "Who this book is for")
    start = doc.append(root, SECTION, "Getting Started")
    doc.append(start, SECTION, "Installation")
    first = doc.append(start, SECTION, "First Program")
    doc.append(first, PARAGRAPH, "Hello, world")
    build = doc.append(first, SECTION, "Building")
    doc.append(build, PARAGRAPH, "karac build vs karac run")
    tour = doc.append(root, SECTION, "Language Tour")
    types = doc.append(tour, SECTION, "Types")
    doc.append(types, SECTION, "Structs")
    doc.append(types, SECTION, "Enums")
    doc.append(tour, SECTION, "Ownership")
    effects = doc.append(tour, SECTION, "Effects")
    doc.append(effects, PARAGRAPH, "Eight built-in verbs")
    doc.append(root, SECTION, "Appendix")
    doc.insert_at(root, 0, PARAGRAPH, "Preface note")
    return doc


def main():
    doc = build_handbook()
    print_outline(doc, "initial")

    locate(doc, "Language Tour/Types/Enums")
    locate(doc, "Getting Started/First Program/Building")
    locate(doc, "Language Tour/Generics")

    effects = doc.find_by_path("Language Tour/Effects")
    start = doc.find_by_path("Getting Started")
    report("move Effects to front of Getting Started", lambda: doc.move_subtree(effects, start, 0))

    tour = doc.find_by_path("Language Tour")
    structs = doc.find_by_path("Language Tour/Types/Structs")
    report("move Language Tour under Structs", lambda: doc.move_subtree(tour, structs, 0))

    para = doc.find_by_path("Introduction/Who this book is for")
    report("add section under a paragraph", lambda: doc.append(para, SECTION, "Audience"))

    appendix = doc.find_by_path("Appendix")
    report("move Appendix to index 9 of root", lambda: doc.move_subtree(appendix, doc.root, 9))
    report("move Appendix to index 1 of root", lambda: doc.move_subtree(appendix, doc.root, 1))
    report("move root", lambda: doc.move_subtree(doc.root, appendix, 0))

    types = doc.find_by_path("Language Tour/Types")
    report("move Types to end of Language Tour", lambda: doc.move_subtree(types, tour, 1))

    first = doc.find_by_path("Getting Started/First Program")
    try:
        n = doc.delete(first)
        print(f"[ok]   delete First Program: removed {n} nodes")
    except TreeError as e:
        print(f"[fail] delete First Program: {e}")
    try:
        n = doc.delete(first)
        print(f"[ok]   delete First Program again: removed {n} nodes")
    except TreeError as e:
        print(f"[fail] delete First Program again: {e}")
    locate(doc, "Getting Started/First Program/Building")
    locate(doc, "Getting Started/Effects/Eight built-in verbs")
    locate(doc, "Language Tour/Types/Structs")

    print_outline(doc, "final")


if __name__ == "__main__":
    main()
