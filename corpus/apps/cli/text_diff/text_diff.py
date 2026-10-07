"""text_diff mirror: LCS line diff with unified-style hunks (context 2)."""

CONTEXT = 2
MAX_LINES = 500

KEEP, INSERT, DELETE = "keep", "insert", "delete"


class DiffError(Exception):
    def __init__(self, kind, name="", lines=0):
        self.kind = kind
        self.name = name
        self.lines = lines

    def describe(self):
        if self.kind == "empty_name":
            return "document has no name"
        return f"{self.name}: {self.lines} lines exceeds limit"


class Edit:
    def __init__(self, kind, old_pos, new_pos, text):
        self.kind = kind
        self.old_pos = old_pos
        self.new_pos = new_pos
        self.text = text

    def prefix(self):
        return {KEEP: " ", INSERT: "+", DELETE: "-"}[self.kind]

    def is_change(self):
        return self.kind != KEEP

    def render(self):
        return self.prefix() + self.text


class Hunk:
    def __init__(self, old_start, new_start):
        self.old_start = old_start
        self.old_len = 0
        self.new_start = new_start
        self.new_len = 0
        self.edits = []

    def header(self):
        return f"@@ -{range_spec(self.old_start, self.old_len)} +{range_spec(self.new_start, self.new_len)} @@"


class Document:
    def __init__(self, name, lines):
        self.name = name
        self.lines = lines

    @staticmethod
    def parse(name, text):
        if name == "":
            raise DiffError("empty_name")
        lines = text.split("\n")
        if len(lines) > 0 and lines[-1] == "":
            lines.pop()
        if len(lines) > MAX_LINES:
            raise DiffError("too_large", name, len(lines))
        return Document(name, lines)

    def line_count(self):
        return len(self.lines)


def range_spec(start, length):
    shown = start if length == 0 else start + 1
    if length == 1:
        return f"{shown}"
    return f"{shown},{length}"


def lcs_table(old, new):
    n, m = len(old), len(new)
    table = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n - 1, -1, -1):
        for j in range(m - 1, -1, -1):
            if old[i] == new[j]:
                table[i][j] = table[i + 1][j + 1] + 1
            else:
                down = table[i + 1][j]
                right = table[i][j + 1]
                table[i][j] = down if down >= right else right
    return table


def edit_script(old, new):
    a, b = old.lines, new.lines
    table = lcs_table(a, b)
    edits = []
    i = j = 0
    while i < len(a) and j < len(b):
        if a[i] == b[j]:
            edits.append(Edit(KEEP, i, j, a[i]))
            i += 1
            j += 1
        elif table[i + 1][j] >= table[i][j + 1]:
            edits.append(Edit(DELETE, i, j, a[i]))
            i += 1
        else:
            edits.append(Edit(INSERT, i, j, b[j]))
            j += 1
    while i < len(a):
        edits.append(Edit(DELETE, i, j, a[i]))
        i += 1
    while j < len(b):
        edits.append(Edit(INSERT, i, j, b[j]))
        j += 1
    return edits


def collect_stats(edits):
    stats = {KEEP: 0, INSERT: 0, DELETE: 0}
    for e in edits:
        stats[e.kind] += 1
    return stats


def make_hunk(edits, lo, hi):
    hunk = Hunk(edits[lo].old_pos, edits[lo].new_pos)
    for k in range(lo, hi + 1):
        e = edits[k]
        if e.kind == KEEP:
            hunk.old_len += 1
            hunk.new_len += 1
        elif e.kind == DELETE:
            hunk.old_len += 1
        else:
            hunk.new_len += 1
        hunk.edits.append(Edit(e.kind, e.old_pos, e.new_pos, e.text))
    return hunk


def build_hunks(edits, context):
    changes = [idx for idx, e in enumerate(edits) if e.is_change()]
    hunks = []
    if not changes:
        return hunks
    last = len(edits) - 1
    group_first = group_last = changes[0]
    for nxt in changes[1:]:
        if nxt - group_last - 1 <= 2 * context:
            group_last = nxt
        else:
            lo = max(group_first - context, 0)
            hi = min(group_last + context, last)
            hunks.append(make_hunk(edits, lo, hi))
            group_first = group_last = nxt
    lo = max(group_first - context, 0)
    hi = min(group_last + context, last)
    hunks.append(make_hunk(edits, lo, hi))
    return hunks


def print_diff(old, new):
    print(f"--- a/{old.name} ({old.line_count()} lines)")
    print(f"+++ b/{new.name} ({new.line_count()} lines)")
    edits = edit_script(old, new)
    hunks = build_hunks(edits, CONTEXT)
    if not hunks:
        print("(no differences)")
    for h in hunks:
        print(h.header())
        for e in h.edits:
            print(e.render())
    s = collect_stats(edits)
    print(f"summary: {len(hunks)} hunk(s), {s[INSERT]} insertion(s)(+), {s[DELETE]} deletion(s)(-), {s[KEEP]} unchanged")


def diff_pair(old_name, old_text, new_name, new_text):
    old = Document.parse(old_name, old_text)
    new = Document.parse(new_name, new_text)
    print_diff(old, new)


CONFIG_OLD = '# server configuration\n[server]\nhost = localhost\nport = 8080\nworkers = 4\ntimeout = 30\nkeepalive = 5\n\n[logging]\nlog_level = info\nlog_file = /var/log/app.log\nrotate = daily\nkeep = 7\n\n[http]\nmax_body = 1mb\ncompression = off\ncors = none\n\n[cache]\ncache = memory\ncache_ttl = 60\ncache_size = 256\n\n[retry]\nretry = 3\nbackoff = linear\n# end\n'
CONFIG_NEW = '# server configuration\n[server]\nhost = 0.0.0.0\nport = 8080\nworkers = 8\ntimeout = 30\nkeepalive = 5\n\n[logging]\nlog_level = info\nlog_file = /var/log/app.log\nrotate = daily\nkeep = 7\n\n[http]\nmax_body = 1mb\ncompression = gzip\ncors = none\n\n[cache]\ncache = memory\ncache_ttl = 60\ncache_size = 256\n\n[retry]\nretry = 3\nbackoff = exponential\njitter = on\n# end\n'
STORY_OLD = 'The quick brown fox\njumps over the lazy dog.\nIt was a sunny day.\nBirds were singing.\nThe fox was hungry.\nIt looked for food.\nThe dog slept on.\nNothing else happened.\nA breeze moved the grass.\nClouds drifted east.\nThe farmer came home.\nHe fed the chickens.\nHe closed the gate.\nThe end.\n'
STORY_NEW = 'A quick brown fox\njumps over the lazy dog.\nIt was a sunny day.\nThe fox was hungry.\nIt looked for food.\nIt found an apple.\nThe dog slept on.\nNothing else happened.\nA breeze moved the grass.\nClouds drifted east.\nThe farmer came home.\nHe fed the chickens.\nHe fed the pigs.\nHe closed the gate.\nThe sun went down.\nThe end.\n'


def main():
    pairs = [
        ("config.ini", CONFIG_OLD, "config.ini", CONFIG_NEW),
        ("story.txt", STORY_OLD, "story.txt", STORY_NEW),
        ("", "a\n", "b.txt", "b\n"),
    ]
    failures = 0
    for old_name, old_text, new_name, new_text in pairs:
        try:
            diff_pair(old_name, old_text, new_name, new_text)
        except DiffError as e:
            print(f"error: {e.describe()}")
            failures += 1
        print("")
    print(f"pairs: {len(pairs)}, failed: {failures}")


if __name__ == "__main__":
    main()
