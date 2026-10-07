"""Python mirror of source.orig.kara (word_freq_multi)."""
import sys

TOP_N = 10
MIN_WORD_LEN = 2

STOP_WORDS = {
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "for",
    "is", "are", "was", "were", "with", "as", "it", "its", "that",
    "this", "by", "at", "be", "from", "but", "not", "we", "our",
    "they", "their", "each", "into", "when", "then", "than", "so",
}

CORPUS = [
    ("wf_harbor.txt", "The harbor wakes before the sun. Fishing boats leave the harbor in a slow line,\nand the gulls follow the boats out to sea. By noon the boats return with fish,\nand the market by the harbor fills with buyers. Fish, ice, rope and salt:\nthe harbor trades in all of them, and the boats never stop."),
    ("wf_garden.txt", "A garden needs water, light and patience. The tomatoes climb the fence;\nthe beans climb the poles. Water the garden early, before the heat.\nWeeds grow faster than tomatoes, so pull the weeds every morning.\nBy August the garden gives more tomatoes than the kitchen can use."),
    ("wf_server.txt", "The server accepts a request, parses the request, and writes a response.\nEach request carries headers; each response carries headers and a body.\nWhen the server is slow, requests queue, and the queue grows.\nA healthy server drains the queue faster than requests arrive."),
    ("wf_library.txt", "The library keeps books, maps and old newspapers. Readers borrow books\nand return books; the library tracks every loan. Quiet readers fill\nthe reading room. The library opens at nine and the readers arrive early,\nwaiting for the doors, waiting for the books."),
    ("wf_kitchen.txt", "In the kitchen the water boils and the bread rises. Salt the water,\nadd the pasta, stir the sauce. The kitchen smells of bread and garlic.\nTomatoes from the garden go into the sauce; fish from the harbor goes\non the grill. The kitchen is never quiet at dinner."),
]


class FreqTable:
    def __init__(self, label):
        self.label = label
        self.counts = {}
        self.total_tokens = 0
        self.kept_tokens = 0

    def add(self, word, n):
        self.counts[word] = self.counts.get(word, 0) + n

    def absorb(self, other):
        for word, count in other.counts.items():
            self.add(word, count)
        self.total_tokens += other.total_tokens
        self.kept_tokens += other.kept_tokens

    def unique(self):
        return len(self.counts)

    def top(self, n):
        keys = sorted((-count, word) for word, count in self.counts.items())
        return [(word, -neg) for neg, word in keys[:n]]


def normalize(text):
    out = text.lower()
    for sep in [",", ".", ";", ":", "!", "?", "(", ")", "\"", "\n", "\t"]:
        out = out.replace(sep, " ")
    return out


def tokenize(text):
    tokens = []
    for piece in normalize(text).split(" "):
        word = piece.strip()
        if word:
            tokens.append(word)
    return tokens


def count_document(name, body):
    table = FreqTable(name)
    for word in tokenize(body):
        table.total_tokens += 1
        if len(word) < MIN_WORD_LEN or word in STOP_WORDS:
            continue
        table.kept_tokens += 1
        table.add(word, 1)
    if table.kept_tokens == 0:
        raise RuntimeError(f"document {name} has no words")
    return table


def print_report(table):
    print(f"== {table.label} ==")
    print(f"tokens: {table.total_tokens}  kept: {table.kept_tokens}  unique: {table.unique()}")
    for rank, (word, count) in enumerate(table.top(TOP_N), start=1):
        print(f"  {rank}. {word} {count}")
    print("")


def main():
    for name, body in CORPUS:
        with open(name, "w") as f:
            f.write(body)
    print(f"wrote {len(CORPUS)} files")
    print("")
    loaded = []
    for name, _ in CORPUS:
        with open(name) as f:
            loaded.append((name, f.read()))
    overall = FreqTable("ALL FILES")
    for name, body in loaded:
        table = count_document(name, body)
        print_report(table)
        overall.absorb(table)
    print_report(overall)


if __name__ == "__main__":
    main()
