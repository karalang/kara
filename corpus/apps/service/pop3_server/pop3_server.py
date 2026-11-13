# Oracle for pop3_server.kara: the same POP3 server (RFC 1939, with CAPA and
# response codes from RFC 2449) and the same scripted clients, written with
# socket and threads. Run: python3 -I -B pop3_server.py > expected.out
import socket
import threading

ADDR = ("127.0.0.1", 47770)
MAX_LINE = 255
MAX_FAILS = 3
SESSIONS = 6

USERS = {"alice": "wonderland", "bob": "builder"}


def message(n, sender, subject, body):
    lines = [
        f"From: {sender}",
        "To: inbox@example.test",
        f"Subject: {subject}",
        f"Message-ID: <m{n}@example.test>",
        "",
    ]
    return lines + body


def seed():
    alice = [
        message(1, "carol@example.test", "Lunch on Friday?", [
            "Are you free for lunch on Friday?",
            "The usual place, at noon.",
        ]),
        message(2, "dave@example.test", "Dots and signatures", [
            "Lines that start with a dot are stuffed on the wire.",
            ".hidden line one",
            ".",
            "..two dots already",
            "-- ",
            "Dave",
        ]),
        message(3, "erin@example.test", "Quarterly numbers", [
            f"Line {i}: revenue {i * 1250} units" for i in range(1, 9)
        ]),
        message(4, "frank@example.test", "Unsubscribe", [
            "Please remove me from the list.",
        ]),
    ]
    bob = [
        message(5, "alice@example.test", "Kara build", [
            "The build is green again.",
            "",
            "Alice",
        ]),
    ]
    return {"alice": alice, "bob": bob}


def size(msg):
    return sum(len(line) + 2 for line in msg)


def uid(msg):
    h = 0xcbf29ce484222325
    for line in msg:
        for b in (line + "\r\n").encode():
            h ^= b
            h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


class Drop:
    def __init__(self, msgs):
        self.msgs = msgs
        self.locked = False


class State:
    def __init__(self):
        self.drops = {u: Drop(m) for u, m in seed().items()}
        self.counts = {}


STATE = State()
LOCK = threading.Lock()


def stuffed(lines):
    return [("." + l) if l.startswith(".") else l for l in lines]


class Session:
    def __init__(self):
        self.phase = "auth"
        self.user = None
        self.fails = 0
        self.deleted = set()
        self.closing = False

    def live(self, drop):
        return [i for i in range(1, len(drop.msgs) + 1) if i not in self.deleted]

    def msg_arg(self, drop, arg):
        if not arg.isdigit():
            return None, "-ERR invalid message number"
        n = int(arg)
        if n < 1 or n > len(drop.msgs):
            return None, "-ERR no such message"
        if n in self.deleted:
            return None, f"-ERR message {n} already deleted"
        return n, None

    def handle(self, line):
        # Returns the reply lines; sets self.closing when the session ends.
        if len(line) > MAX_LINE:
            return ["-ERR line too long"]
        parts = line.split(" ")
        verb = parts[0].upper()
        args = [p for p in parts[1:] if p != ""]
        STATE.counts[verb] = STATE.counts.get(verb, 0) + 1
        if verb == "CAPA":
            return ["+OK Capability list follows", "TOP", "USER", "UIDL",
                    "RESP-CODES", "IMPLEMENTATION kara-pop3", "."]
        if verb == "QUIT":
            self.closing = True
            if self.phase == "trans":
                drop = STATE.drops[self.user]
                drop.msgs = [m for i, m in enumerate(drop.msgs, 1) if i not in self.deleted]
                drop.locked = False
                return [f"+OK kara POP3 server signing off ({len(drop.msgs)} messages left)"]
            return ["+OK kara POP3 server signing off"]
        if verb == "NOOP" and self.phase == "trans":
            return ["+OK"]
        if self.phase == "auth":
            if verb == "USER":
                if len(args) != 1:
                    return ["-ERR USER takes one argument"]
                self.user = args[0]
                return [f"+OK send PASS for {args[0]}"]
            if verb == "PASS":
                if self.user is None:
                    return ["-ERR USER first"]
                if USERS.get(self.user) != " ".join(args):
                    self.fails += 1
                    self.user = None
                    if self.fails >= MAX_FAILS:
                        self.closing = True
                        return ["-ERR [AUTH] too many failures, goodbye"]
                    return ["-ERR [AUTH] invalid credentials"]
                drop = STATE.drops[self.user]
                if drop.locked:
                    self.user = None
                    return ["-ERR [IN-USE] maildrop already locked"]
                drop.locked = True
                self.phase = "trans"
                n = len(drop.msgs)
                return [f"+OK {self.user} has {n} messages ({sum(size(m) for m in drop.msgs)} octets)"]
            if verb in ("STAT", "LIST", "UIDL", "RETR", "TOP", "DELE", "RSET", "NOOP"):
                return ["-ERR command not valid in this state"]
            return ["-ERR unknown command"]
        drop = STATE.drops[self.user]
        if verb == "STAT":
            live = self.live(drop)
            return [f"+OK {len(live)} {sum(size(drop.msgs[i - 1]) for i in live)}"]
        if verb in ("LIST", "UIDL"):
            show = (lambda i: f"{i} {size(drop.msgs[i - 1])}") if verb == "LIST" \
                else (lambda i: f"{i} {uid(drop.msgs[i - 1])}")
            if args:
                n, err = self.msg_arg(drop, args[0])
                return [err] if err else ["+OK " + show(n)]
            live = self.live(drop)
            head = f"+OK {len(live)} messages" if verb == "LIST" else "+OK unique-id listing follows"
            return [head] + [show(i) for i in live] + ["."]
        if verb in ("RETR", "TOP"):
            want = 2 if verb == "TOP" else 1
            if len(args) != want:
                return [f"-ERR {verb} takes {want} argument{'s' if want > 1 else ''}"]
            n, err = self.msg_arg(drop, args[0])
            if err:
                return [err]
            msg = drop.msgs[n - 1]
            if verb == "RETR":
                return [f"+OK {size(msg)} octets"] + stuffed(msg) + ["."]
            if not args[1].isdigit():
                return ["-ERR invalid line count"]
            k = int(args[1])
            blank = msg.index("")
            return ["+OK top of message follows"] + stuffed(msg[:blank + 1 + k]) + ["."]
        if verb == "DELE":
            if len(args) != 1:
                return ["-ERR DELE takes 1 argument"]
            n, err = self.msg_arg(drop, args[0])
            if err:
                return [err]
            self.deleted.add(n)
            return [f"+OK message {n} deleted"]
        if verb == "RSET":
            self.deleted.clear()
            return [f"+OK maildrop has {len(drop.msgs)} messages ({sum(size(m) for m in drop.msgs)} octets)"]
        if verb in ("USER", "PASS"):
            return ["-ERR command not valid in this state"]
        return ["-ERR unknown command"]

    def abandon(self):
        if self.phase == "trans" and not self.closing:
            STATE.drops[self.user].locked = False


def serve(conn):
    s = Session()
    out = conn.makefile("wb")
    inp = conn.makefile("rb")
    out.write(b"+OK kara POP3 server ready\r\n")
    out.flush()
    for raw in inp:
        line = raw.decode().rstrip("\r\n")
        with LOCK:
            replies = s.handle(line)
        out.write("".join(r + "\r\n" for r in replies).encode())
        out.flush()
        if s.closing:
            break
    with LOCK:
        s.abandon()
    out.close()
    inp.close()
    conn.close()


def run_server(listener):
    tasks = []
    for _ in range(SESSIONS):
        conn, _ = listener.accept()
        t = threading.Thread(target=serve, args=(conn,))
        t.start()
        tasks.append(t)
    for t in tasks:
        t.join()


class Client:
    def __init__(self, name):
        self.name = name
        self.sock = socket.create_connection(ADDR)
        self.sock.settimeout(2.0)
        self.inp = self.sock.makefile("rb")
        self.read_line()

    def read_line(self):
        raw = self.inp.readline()
        if raw == b"":
            print(f"{self.name} <- (closed)")
            return None
        line = raw.decode().rstrip("\r\n")
        print(f"{self.name} <- {line}")
        return line

    def cmd(self, line, multi=False):
        shown = line if len(line) <= 40 else f"{line[:20]}... ({len(line)} octets)"
        print(f"{self.name} -> {shown}")
        self.sock.sendall((line + "\r\n").encode())
        first = self.read_line()
        body = []
        if multi and first is not None and first.startswith("+OK"):
            while True:
                l = self.read_line()
                if l is None or l == ".":
                    break
                body.append(l[1:] if l.startswith(".") else l)
        return first, body

    def retr(self, n):
        first, body = self.cmd(f"RETR {n}", multi=True)
        if not first.startswith("+OK"):
            return
        claimed = int(first.split(" ")[1])
        octets = sum(len(l) + 2 for l in body)
        subject = next((l[9:] for l in body if l.startswith("Subject: ")), "?")
        blank = body.index("")
        print(f"{self.name} :: message {n}: {len(body) - blank - 1} body lines, "
              f"{octets} octets (server said {claimed}, {'ok' if octets == claimed else 'MISMATCH'}), "
              f"subject \"{subject}\"")

    def expect_close(self):
        raw = self.inp.readline()
        print(f"{self.name} <- " + ("(closed)" if raw == b"" else f"unexpected {raw!r}"))

    def hang_up(self):
        print(f"{self.name} -- hangs up without QUIT")
        self.sock.shutdown(socket.SHUT_WR)
        self.expect_close()

    def close(self):
        self.inp.close()
        self.sock.close()


def main():
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(ADDR)
    listener.listen(8)
    server = threading.Thread(target=run_server, args=(listener,))
    server.start()

    print("== session 1: alice reads and deletes")
    a = Client("alice1")
    a.cmd("CAPA", multi=True)
    a.cmd("STAT")
    a.cmd("PASS wonderland")
    a.cmd("USER alice")
    a.cmd("PASS rabbit-hole")
    a.cmd("USER alice")
    a.cmd("PASS wonderland")
    a.cmd("STAT")
    a.cmd("LIST", multi=True)
    a.cmd("LIST 3")
    a.cmd("LIST 9")
    a.cmd("UIDL", multi=True)
    a.retr(2)
    a.cmd("TOP 3 2", multi=True)
    a.cmd("DELE 1")
    a.cmd("DELE 1")
    a.cmd("LIST 1")
    a.cmd("STAT")
    a.cmd("RSET")
    a.cmd("STAT")
    a.cmd("DELE 4")

    print("== session 2: alice again while the maildrop is locked")
    b = Client("alice2")
    b.cmd("USER alice")
    b.cmd("PASS wonderland")
    b.cmd("QUIT")
    b.expect_close()
    b.close()

    a.cmd("NOOP")
    a.cmd("QUIT")
    a.expect_close()
    a.close()

    print("== session 3: deletes, then hangs up without QUIT")
    c = Client("alice3")
    c.cmd("USER alice")
    c.cmd("PASS wonderland")
    c.cmd("UIDL", multi=True)
    c.cmd("DELE 1")
    c.cmd("DELE 2")
    c.hang_up()
    c.close()

    print("== session 4: nothing was deleted")
    d = Client("alice4")
    d.cmd("USER alice")
    d.cmd("PASS wonderland")
    d.cmd("STAT")
    d.retr(1)
    d.cmd("QUIT")
    d.expect_close()
    d.close()

    print("== session 5: bob guesses")
    e = Client("bob1")
    for pw in ["build", "Builder", "bob"]:
        e.cmd("USER bob")
        e.cmd(f"PASS {pw}")
    e.expect_close()
    e.close()

    print("== session 6: bob and the error replies")
    f = Client("bob2")
    f.cmd("USER bob")
    f.cmd("PASS builder")
    f.cmd("USER bob")
    f.cmd("RETR")
    f.cmd("RETR abc")
    f.cmd("RETR 0")
    f.cmd("TOP 1")
    f.cmd("TOP 1 x")
    f.cmd("XYZZY")
    f.cmd("NOOP " + "x" * 300)
    f.retr(1)
    f.cmd("TOP 1 0", multi=True)
    f.cmd("DELE 1")
    f.cmd("STAT")
    f.cmd("QUIT")
    f.expect_close()
    f.close()

    server.join()
    listener.close()
    print("== server")
    for verb in sorted(STATE.counts):
        print(f"{verb} {STATE.counts[verb]}")
    for user in sorted(STATE.drops):
        drop = STATE.drops[user]
        print(f"{user}: {len(drop.msgs)} messages, locked {'yes' if drop.locked else 'no'}")
        for m in drop.msgs:
            subject = next(l[9:] for l in m if l.startswith("Subject: "))
            print(f"  {uid(m)} {size(m)} {subject}")


main()
