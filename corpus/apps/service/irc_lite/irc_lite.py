"""Oracle for irc_lite.kara: the same IRC-style chat server over real sockets.

The server is plain `socket` code with a thread per connection and one lock
over its state; the scripted clients run on the main thread, each step
reading exactly the lines the server owes it.
"""

import socket
import threading

ADDR = ("127.0.0.1", 47760)
SERVER = "kara.irc"
MAX_LINE = 510
CLIENTS = 3


class Message:
    def __init__(self, prefix, command, params):
        self.prefix = prefix
        self.command = command
        self.params = params


def parse_line(line):
    """One IRC line (without its CRLF): an optional `:prefix`, a command
    and up to 15 parameters, the last of which may be a `:trailing` one
    holding spaces."""
    rest = line
    prefix = None
    if rest.startswith(":"):
        sp = rest.find(" ")
        if sp < 0:
            return None
        prefix = rest[1:sp]
        rest = rest[sp + 1:]
    rest = rest.lstrip(" ")
    if rest == "":
        return None
    sp = rest.find(" ")
    if sp < 0:
        command, rest = rest, ""
    else:
        command, rest = rest[:sp], rest[sp + 1:]
    params = []
    while rest != "" and len(params) < 15:
        if rest.startswith(" "):
            rest = rest[1:]
            continue
        if rest.startswith(":"):
            params.append(rest[1:])
            break
        sp = rest.find(" ")
        if sp < 0:
            params.append(rest)
            break
        params.append(rest[:sp])
        rest = rest[sp + 1:]
    return Message(prefix, command.upper(), params)


def valid_nick(n):
    if len(n) == 0 or len(n) > 9 or not n[0].isascii() or not n[0].isalpha():
        return False
    return all((c.isascii() and c.isalnum()) or c in "-_[]\\`^{}|" for c in n)


class Client:
    def __init__(self, cid, out):
        self.id = cid
        self.out = out
        self.nick = None
        self.user = None
        self.real = ""
        self.registered = False
        self.channels = []


class Channel:
    def __init__(self):
        self.members = []
        self.ops = []
        self.topic = None


class State:
    def __init__(self):
        self.clients = {}
        self.nicks = {}
        self.channels = {}
        self.counts = {}
        self.lock = threading.Lock()


def mask(c):
    return f"{c.nick}!{c.user}@localhost"


def send(st, cid, line):
    c = st.clients.get(cid)
    if c is not None:
        try:
            c.out.sendall((line + "\r\n").encode())
        except OSError:
            pass


def reply(st, cid, code, text):
    c = st.clients[cid]
    nick = c.nick if c.nick is not None else "*"
    send(st, cid, f":{SERVER} {code} {nick} {text}")


def peers(st, cid):
    """The clients sharing a channel with `cid`, each once, by id."""
    out = []
    for name in st.clients[cid].channels:
        for m in st.channels[name].members:
            if m != cid and m not in out:
                out.append(m)
    out.sort()
    return out


def names_line(st, name):
    ch = st.channels[name]
    shown = []
    for m in ch.members:
        p = "@" if m in ch.ops else ""
        shown.append(p + st.clients[m].nick)
    return " ".join(shown)


def leave(st, cid, name):
    ch = st.channels[name]
    ch.members.remove(cid)
    if cid in ch.ops:
        ch.ops.remove(cid)
    st.clients[cid].channels.remove(name)
    if not ch.members:
        del st.channels[name]


def quit_client(st, cid, reason):
    c = st.clients[cid]
    if c.registered:
        for p in peers(st, cid):
            send(st, p, f":{mask(c)} QUIT :{reason}")
    for name in list(c.channels):
        leave(st, cid, name)
    if c.nick is not None:
        del st.nicks[c.nick.lower()]
    del st.clients[cid]


def welcome(st, cid):
    c = st.clients[cid]
    if not c.registered and c.nick is not None and c.user is not None:
        c.registered = True
        reply(st, cid, "001", f":Welcome to {SERVER}, {mask(c)}")


def handle(st, cid, m):
    """Runs one command; returns whether the connection ends."""
    st.counts[m.command] = st.counts.get(m.command, 0) + 1
    c = st.clients[cid]
    p = m.params
    cmd = m.command
    if cmd == "PING":
        token = p[0] if p else ""
        send(st, cid, f":{SERVER} PONG {SERVER} :{token}")
        return False
    if cmd == "QUIT":
        reason = p[0] if p else "Client Quit"
        send(st, cid, f"ERROR :Closing link ({reason})")
        quit_client(st, cid, f"Quit: {reason}")
        return True
    if cmd == "NICK":
        if not p:
            reply(st, cid, "431", ":No nickname given")
            return False
        new = p[0]
        if not valid_nick(new):
            reply(st, cid, "432", f"{new} :Erroneous nickname")
            return False
        owner = st.nicks.get(new.lower())
        if owner is not None and owner != cid:
            reply(st, cid, "433", f"{new} :Nickname is already in use")
            return False
        if c.registered:
            line = f":{mask(c)} NICK {new}"
            send(st, cid, line)
            for q in peers(st, cid):
                send(st, q, line)
        if c.nick is not None:
            del st.nicks[c.nick.lower()]
        c.nick = new
        st.nicks[new.lower()] = cid
        welcome(st, cid)
        return False
    if cmd == "USER":
        if len(p) < 4:
            reply(st, cid, "461", "USER :Not enough parameters")
            return False
        if c.registered:
            reply(st, cid, "462", ":You may not reregister")
            return False
        c.user = p[0]
        c.real = p[3]
        welcome(st, cid)
        return False
    if not c.registered:
        reply(st, cid, "451", ":You have not registered")
        return False
    if cmd == "JOIN":
        if not p:
            reply(st, cid, "461", "JOIN :Not enough parameters")
            return False
        for name in p[0].split(","):
            if len(name) < 2 or not name.startswith("#"):
                reply(st, cid, "403", f"{name} :No such channel")
                continue
            if name in c.channels:
                continue
            ch = st.channels.get(name)
            if ch is None:
                ch = Channel()
                ch.ops.append(cid)
                st.channels[name] = ch
            ch.members.append(cid)
            c.channels.append(name)
            for q in ch.members:
                send(st, q, f":{mask(c)} JOIN {name}")
            if ch.topic is not None:
                reply(st, cid, "332", f"{name} :{ch.topic}")
            reply(st, cid, "353", f"= {name} :{names_line(st, name)}")
            reply(st, cid, "366", f"{name} :End of /NAMES list")
        return False
    if cmd == "PART":
        if not p:
            reply(st, cid, "461", "PART :Not enough parameters")
            return False
        name = p[0]
        if name not in st.channels:
            reply(st, cid, "403", f"{name} :No such channel")
            return False
        if name not in c.channels:
            reply(st, cid, "442", f"{name} :You're not on that channel")
            return False
        line = f":{mask(c)} PART {name}"
        if len(p) > 1:
            line = f"{line} :{p[1]}"
        for q in st.channels[name].members:
            send(st, q, line)
        leave(st, cid, name)
        return False
    if cmd == "PRIVMSG":
        if not p:
            reply(st, cid, "411", ":No recipient given (PRIVMSG)")
            return False
        if len(p) < 2 or p[1] == "":
            reply(st, cid, "412", ":No text to send")
            return False
        target = p[0]
        line = f":{mask(c)} PRIVMSG {target} :{p[1]}"
        if target.startswith("#"):
            ch = st.channels.get(target)
            if ch is None:
                reply(st, cid, "403", f"{target} :No such channel")
            elif cid not in ch.members:
                reply(st, cid, "404", f"{target} :Cannot send to channel")
            else:
                for q in ch.members:
                    if q != cid:
                        send(st, q, line)
            return False
        to = st.nicks.get(target.lower())
        if to is None:
            reply(st, cid, "401", f"{target} :No such nick/channel")
        else:
            send(st, to, line)
        return False
    if cmd == "TOPIC":
        if not p:
            reply(st, cid, "461", "TOPIC :Not enough parameters")
            return False
        name = p[0]
        ch = st.channels.get(name)
        if ch is None:
            reply(st, cid, "403", f"{name} :No such channel")
            return False
        if cid not in ch.members:
            reply(st, cid, "442", f"{name} :You're not on that channel")
            return False
        if len(p) < 2:
            if ch.topic is None:
                reply(st, cid, "331", f"{name} :No topic is set")
            else:
                reply(st, cid, "332", f"{name} :{ch.topic}")
            return False
        if cid not in ch.ops:
            reply(st, cid, "482", f"{name} :You're not channel operator")
            return False
        ch.topic = p[1]
        for q in ch.members:
            send(st, q, f":{mask(c)} TOPIC {name} :{p[1]}")
        return False
    if cmd == "NAMES":
        if p and p[0] in st.channels:
            reply(st, cid, "353", f"= {p[0]} :{names_line(st, p[0])}")
        name = p[0] if p else "*"
        reply(st, cid, "366", f"{name} :End of /NAMES list")
        return False
    reply(st, cid, "421", f"{cmd} :Unknown command")
    return False


def serve(st, cid, conn):
    f = conn.makefile("rb")
    ended = False
    for raw in f:
        line = raw.decode("utf-8", "replace")
        if line.endswith("\n"):
            line = line[:-1]
            if line.endswith("\r"):
                line = line[:-1]
        with st.lock:
            if len(line) > MAX_LINE:
                reply(st, cid, "417", ":Input line was too long")
                continue
            m = parse_line(line)
            if m is None:
                continue
            if handle(st, cid, m):
                ended = True
                break
    if not ended:
        with st.lock:
            quit_client(st, cid, "Connection closed")
    f.close()
    conn.close()


def run_server(st, listener):
    threads = []
    for cid in range(1, CLIENTS + 1):
        conn, _ = listener.accept()
        with st.lock:
            st.clients[cid] = Client(cid, conn)
        t = threading.Thread(target=serve, args=(st, cid, conn))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()


class Peer:
    def __init__(self, name):
        self.name = name
        self.sock = socket.create_connection(ADDR)
        self.sock.settimeout(2.0)
        self.inp = self.sock.makefile("rb")

    def say(self, line):
        self.sock.sendall((line + "\r\n").encode())

    def expect(self, n):
        for _ in range(n):
            raw = self.inp.readline()
            if raw == b"":
                print(f"{self.name} <- (closed)")
                return
            line = raw.decode()
            if line.endswith("\n"):
                line = line[:-1]
                if line.endswith("\r"):
                    line = line[:-1]
            print(f"{self.name} <- {line}")

    def expect_close(self):
        raw = self.inp.readline()
        if raw == b"":
            print(f"{self.name} <- (closed)")
        else:
            print(f"{self.name} <- unexpected {raw.decode().rstrip()}")

    def close(self):
        self.inp.close()
        self.sock.close()


def show(m):
    if m is None:
        return "(none)"
    prefix = m.prefix if m.prefix is not None else "-"
    params = "".join(f"[{x}]" for x in m.params)
    return f"{m.command} prefix={prefix} params={params}"


def self_test():
    print("== parse")
    for line in [
        "NICK alice",
        ":irc.example PRIVMSG #kara :hello  world ",
        "privmsg bob :",
        "JOIN   #a,#b",
        "PING",
        "",
        ":only-prefix",
        "USER bob 0 * :Bob Smith",
        "MODE #kara +o alice",
        "a b c d e f g h i j k l m n o p q r",
    ]:
        print(f"\"{line}\" -> {show(parse_line(line))}")
    print("== nicks")
    for n in ["alice", "9lives", "a_b-c[1]", "toolongnick", "", "bob smith", "x|y"]:
        print(f"\"{n}\" {'ok' if valid_nick(n) else 'bad'}")


def main():
    self_test()
    print("== live")
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(ADDR)
    listener.listen(8)
    st = State()
    server = threading.Thread(target=run_server, args=(st, listener))
    server.start()

    alice = Peer("alice")
    alice.say("NICK alice")
    alice.say("USER alice 0 * :Alice Liddell")
    alice.expect(1)

    bob = Peer("bob")
    bob.say("PRIVMSG alice :hi")
    bob.expect(1)
    bob.say("NICK Alice")
    bob.expect(1)
    bob.say("NICK 9bob")
    bob.expect(1)
    bob.say("NICK bob")
    bob.say("USER bob 0 * :Bob")
    bob.expect(1)

    carol = Peer("carol")
    carol.say("NICK carol")
    carol.say("USER carol 0 * :Carol")
    carol.expect(1)
    carol.say("USER carol 0 * :Again")
    carol.expect(1)

    alice.say("JOIN #kara")
    alice.expect(3)
    bob.say("JOIN #kara,#rust,x")
    bob.expect(7)
    alice.expect(1)
    alice.say("TOPIC #kara :Kara dev chat")
    alice.expect(1)
    bob.expect(1)
    bob.say("TOPIC #kara :mine now")
    bob.expect(1)
    bob.say("TOPIC #kara")
    bob.expect(1)
    carol.say("JOIN #kara")
    carol.expect(4)
    alice.expect(1)
    bob.expect(1)

    alice.say("PRIVMSG #kara :hello  everyone ")
    bob.expect(1)
    carol.expect(1)
    carol.say("PRIVMSG #rust :hi")
    carol.expect(1)
    carol.say("PRIVMSG BOB :psst")
    bob.expect(1)
    carol.say("PRIVMSG dave :x")
    carol.expect(1)
    carol.say("PRIVMSG")
    carol.expect(1)
    carol.say("PRIVMSG bob")
    carol.expect(1)
    carol.say("PRIVMSG #kara :" + "x" * 600)
    carol.expect(1)

    bob.say("NICK robert")
    bob.expect(1)
    alice.expect(1)
    carol.expect(1)
    alice.say("PING :t1")
    alice.expect(1)
    alice.say("WHO #kara")
    alice.expect(1)
    carol.say("PART #rust")
    carol.expect(1)
    bob.say("PART #rust :bye")
    bob.expect(1)
    carol.say("NAMES #rust")
    carol.expect(1)
    alice.say("NAMES #kara")
    alice.expect(2)

    bob.close()
    alice.expect(1)
    carol.expect(1)
    carol.say("QUIT :gone home")
    carol.expect(1)
    carol.expect_close()
    alice.expect(1)
    alice.say("QUIT")
    alice.expect(1)
    alice.expect_close()
    carol.close()
    alice.close()

    server.join()
    listener.close()
    print("== server")
    for k in sorted(st.counts):
        print(f"{k} {st.counts[k]}")
    print(f"clients {len(st.clients)} channels {len(st.channels)} nicks {len(st.nicks)}")


main()
