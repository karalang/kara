# Oracle for ftp_server.kara: the same FTP server (RFC 959, with PASV, EPSV
# from RFC 2428 and SIZE/FEAT from RFC 3659 and 2389) over an in-memory file
# tree, and the same scripted clients, written with socket and threads.
# Run: python3 -I -B ftp_server.py > expected.out
import socket
import threading

ADDR = ("127.0.0.1", 47780)
SESSIONS = 3

USERS = {"dev": "s3cret"}


def seed():
    files = {
        "/readme.txt": b"Welcome to the kara FTP server.\nAnonymous users may read.\n",
        "/pub/notes.txt": b"line one\nline two\nline three\n",
        "/pub/data.csv": b"id,name\n1,ada\n2,grace\n",
    }
    dirs = {"/", "/pub", "/incoming"}
    return files, dirs


class State:
    def __init__(self):
        self.files, self.dirs = seed()
        self.counts = {}
        self.lock = threading.Lock()


def parent(path):
    i = path.rfind("/")
    return "/" if i == 0 else path[:i]


def base(path):
    return path[path.rfind("/") + 1:]


def resolve(cwd, arg):
    start = [] if arg.startswith("/") else [p for p in cwd.split("/") if p]
    for part in arg.split("/"):
        if part == "" or part == ".":
            continue
        if part == "..":
            if start:
                start.pop()
        else:
            start.append(part)
    return "/" + "/".join(start)


class Session:
    def __init__(self):
        self.user = None
        self.pending = None
        self.logged_in = False
        self.anonymous = False
        self.cwd = "/"
        self.binary = False
        self.passive = None
        self.rename_from = None
        self.closing = False


def to_ascii(data):
    return data.replace(b"\n", b"\r\n")


def from_ascii(data):
    return data.replace(b"\r\n", b"\n")


def listing(st, path, names_only):
    lines = []
    prefix = path if path == "/" else path + "/"
    entries = sorted(
        {p[len(prefix):].split("/")[0] for p in list(st.files) + list(st.dirs)
         if p.startswith(prefix) and p != path and p != "/"}
    )
    for name in entries:
        full = prefix + name
        if names_only:
            lines.append(name)
        elif full in st.dirs:
            lines.append(f"drwxr-xr-x 1 ftp ftp {0:>8} {name}")
        else:
            lines.append(f"-rw-r--r-- 1 ftp ftp {len(st.files[full]):>8} {name}")
    return "".join(l + "\r\n" for l in lines).encode()


def open_passive(s):
    ln = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    ln.bind(("127.0.0.1", 0))
    ln.listen(1)
    s.passive = ln
    return ln.getsockname()[1]


def take_data(s):
    ln = s.passive
    s.passive = None
    conn, _ = ln.accept()
    ln.close()
    return conn


def handle(st, s, line, send):
    verb, _, arg = line.partition(" ")
    verb = verb.upper()
    st.counts[verb] = st.counts.get(verb, 0) + 1
    if verb == "QUIT":
        s.closing = True
        return send("221 Goodbye")
    if verb == "NOOP":
        return send("200 NOOP ok")
    if verb == "SYST":
        return send("215 UNIX Type: L8")
    if verb == "FEAT":
        return send("211-Features:\r\n EPSV\r\n PASV\r\n SIZE\r\n UTF8\r\n211 End")
    if verb == "USER":
        if not arg:
            return send("501 Syntax error in parameters or arguments")
        s.user = arg
        s.logged_in = False
        if arg == "anonymous":
            return send("331 Anonymous login ok, send your email as password")
        return send(f"331 Password required for {arg}")
    if verb == "PASS":
        if s.user is None:
            return send("503 Login with USER first")
        if s.user == "anonymous":
            s.logged_in = True
            s.anonymous = True
            return send("230 Anonymous access granted, restrictions apply")
        if USERS.get(s.user) == arg:
            s.logged_in = True
            s.anonymous = False
            return send(f"230 User {s.user} logged in")
        s.user = None
        return send("530 Login incorrect")
    if not s.logged_in:
        return send("530 Please login with USER and PASS")
    if verb == "PWD":
        return send(f'257 "{s.cwd}" is the current directory')
    if verb in ("CWD", "CDUP"):
        target = resolve(s.cwd, ".." if verb == "CDUP" else arg)
        if target not in st.dirs:
            return send(f"550 {target}: No such directory")
        s.cwd = target
        return send(f"250 Directory changed to {target}")
    if verb == "TYPE":
        if arg.upper() == "I":
            s.binary = True
            return send("200 Type set to I")
        if arg.upper() == "A":
            s.binary = False
            return send("200 Type set to A")
        return send("504 Command not implemented for that parameter")
    if verb == "PASV":
        port = open_passive(s)
        return send(f"227 Entering Passive Mode (127,0,0,1,{port // 256},{port % 256})")
    if verb == "EPSV":
        port = open_passive(s)
        return send(f"229 Entering Extended Passive Mode (|||{port}|)")
    if verb in ("MKD", "RMD", "DELE", "RNFR", "RNTO", "SIZE", "RETR", "STOR", "APPE") and not arg:
        return send("501 Syntax error in parameters or arguments")
    path = resolve(s.cwd, arg) if arg else s.cwd
    writes = ("MKD", "RMD", "DELE", "RNFR", "RNTO", "STOR", "APPE")
    if verb in writes and s.anonymous:
        if verb in ("STOR", "APPE") and s.passive is not None:
            s.passive.close()
            s.passive = None
        return send("550 Permission denied")
    if verb == "SIZE":
        if path not in st.files:
            return send(f"550 {path}: No such file")
        if not s.binary:
            return send("550 SIZE not allowed in ASCII mode")
        return send(f"213 {len(st.files[path])}")
    if verb == "MKD":
        if path in st.dirs or path in st.files:
            return send(f"550 {path}: File exists")
        if parent(path) not in st.dirs:
            return send(f"550 {parent(path)}: No such directory")
        st.dirs.add(path)
        return send(f'257 "{path}" created')
    if verb == "RMD":
        if path == "/":
            return send("550 Cannot remove the root directory")
        if path not in st.dirs:
            return send(f"550 {path}: No such directory")
        if any(p.startswith(path + "/") for p in list(st.files) + list(st.dirs)):
            return send(f"550 {path}: Directory not empty")
        st.dirs.discard(path)
        return send("250 Directory removed")
    if verb == "DELE":
        if path not in st.files:
            return send(f"550 {path}: No such file")
        del st.files[path]
        return send("250 File deleted")
    if verb == "RNFR":
        if path not in st.files:
            return send(f"550 {path}: No such file")
        s.rename_from = path
        return send("350 Ready for RNTO")
    if verb == "RNTO":
        if s.rename_from is None:
            return send("503 Bad sequence of commands")
        src = s.rename_from
        s.rename_from = None
        if parent(path) not in st.dirs:
            return send(f"550 {parent(path)}: No such directory")
        st.files[path] = st.files.pop(src)
        return send("250 Rename successful")
    if verb in ("LIST", "NLST", "RETR", "STOR", "APPE"):
        if s.passive is None:
            return send("425 Use PASV or EPSV first")
        if verb in ("LIST", "NLST"):
            if path not in st.dirs:
                s.passive.close()
                s.passive = None
                return send(f"550 {path}: No such directory")
            send("150 Here comes the directory listing")
            data = take_data(s)
            data.sendall(listing(st, path, verb == "NLST"))
            data.close()
            return send("226 Directory send OK")
        if verb == "RETR":
            if path not in st.files:
                s.passive.close()
                s.passive = None
                return send(f"550 {path}: No such file")
            body = st.files[path]
            if s.binary:
                send(f"150 Opening BINARY mode data connection for {path} ({len(body)} bytes)")
            else:
                send(f"150 Opening ASCII mode data connection for {path}")
                body = to_ascii(body)
            data = take_data(s)
            data.sendall(body)
            data.close()
            return send("226 Transfer complete")
        if parent(path) not in st.dirs:
            s.passive.close()
            s.passive = None
            return send(f"550 {parent(path)}: No such directory")
        send("150 Ok to send data")
        data = take_data(s)
        got = b""
        while True:
            chunk = data.recv(4096)
            if not chunk:
                break
            got += chunk
        data.close()
        if not s.binary:
            got = from_ascii(got)
        if verb == "APPE":
            st.files[path] = st.files.get(path, b"") + got
        else:
            st.files[path] = got
        return send(f"226 Transfer complete ({len(got)} bytes stored)")
    return send("502 Command not implemented")


def serve(st, conn):
    out = conn.makefile("wb")
    reader = conn.makefile("rb")
    s = Session()

    def send(text):
        out.write((text + "\r\n").encode())
        out.flush()

    send("220 kara FTP server ready")
    for raw in reader:
        line = raw.decode().rstrip("\r\n")
        with st.lock:
            handle(st, s, line, send)
        if s.closing:
            break
    if s.passive is not None:
        s.passive.close()
    out.close()
    reader.close()
    conn.close()


def run_server(st, listener):
    threads = []
    for _ in range(SESSIONS):
        conn, _ = listener.accept()
        t = threading.Thread(target=serve, args=(st, conn))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()


# ------------------------------------------------------------ the clients


class Client:
    def __init__(self, name):
        self.name = name
        self.sock = socket.create_connection(ADDR)
        self.sock.settimeout(2.0)
        self.inp = self.sock.makefile("rb")
        self.read_reply()

    def read_line(self):
        raw = self.inp.readline()
        if not raw:
            print(f"{self.name} <- (closed)")
            return None
        return raw.decode().rstrip("\r\n")

    # Reads one reply, multi-line ("211-" .. "211 ") included. A passive
    # reply's port changes from run to run, so it is shown masked.
    def read_reply(self):
        first = self.read_line()
        if first is None:
            return ""
        lines = [first]
        if len(first) > 3 and first[3] == "-":
            while True:
                l = self.read_line()
                if l is None:
                    break
                lines.append(l)
                if l.startswith(first[:3] + " "):
                    break
        for l in lines:
            shown = l
            if l.startswith("227 "):
                shown = l[: l.index("(")] + "(127,0,0,1,*,*)"
            elif l.startswith("229 "):
                shown = l[: l.index("(")] + "(|||*|)"
            print(f"{self.name} <- {shown}")
        return first

    def cmd(self, line):
        print(f"{self.name} -> {line}")
        self.sock.sendall((line + "\r\n").encode())
        return self.read_reply()

    def passive(self, extended):
        reply = self.cmd("EPSV" if extended else "PASV")
        if extended:
            if not reply.startswith("229 "):
                return None
            port = int(reply[reply.index("|||") + 3: reply.rindex("|")])
        else:
            if not reply.startswith("227 "):
                return None
            nums = reply[reply.index("(") + 1: reply.index(")")].split(",")
            port = int(nums[4]) * 256 + int(nums[5])
        return socket.create_connection(("127.0.0.1", port))

    def fetch(self, line, extended=False):
        data = self.passive(extended)
        if data is None:
            return
        first = self.cmd(line)
        if not first.startswith("150"):
            data.close()
            return
        got = b""
        while True:
            chunk = data.recv(4096)
            if not chunk:
                break
            got += chunk
        data.close()
        crlf = got.count(b"\r\n")
        print(f"{self.name} :: {len(got)} bytes, {crlf} CRLF")
        for l in got.decode().split("\n"):
            if l:
                print(f"{self.name} :: | {l.rstrip(chr(13))}")
        self.read_reply()

    def send_file(self, line, body, extended=False):
        data = self.passive(extended)
        if data is None:
            return
        first = self.cmd(line)
        if not first.startswith("150"):
            data.close()
            return
        data.sendall(body)
        data.close()
        print(f"{self.name} :: sent {len(body)} bytes")
        self.read_reply()

    def expect_close(self):
        raw = self.inp.readline()
        if not raw:
            print(f"{self.name} <- (closed)")
        else:
            print(f"{self.name} <- unexpected {raw.decode().rstrip()}")

    def close(self):
        self.inp.close()
        self.sock.close()


def main():
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(ADDR)
    listener.listen(8)
    st = State()
    server = threading.Thread(target=run_server, args=(st, listener))
    server.start()

    print("== session 1: anonymous reads")
    a = Client("anon")
    a.cmd("SYST")
    a.cmd("FEAT")
    a.cmd("PWD")
    a.cmd("LIST")
    a.cmd("USER anonymous")
    a.cmd("PASS guest@example.test")
    a.cmd("PWD")
    a.fetch("LIST")
    a.cmd("CWD pub")
    a.cmd("PWD")
    a.fetch("NLST", extended=True)
    a.fetch("RETR notes.txt")
    a.cmd("TYPE I")
    a.cmd("SIZE notes.txt")
    a.fetch("RETR notes.txt", extended=True)
    a.cmd("TYPE A")
    a.cmd("SIZE notes.txt")
    a.cmd("SIZE missing.txt")
    a.fetch("RETR missing.txt")
    a.cmd("RETR notes.txt")
    a.cmd("CWD /nowhere")
    a.cmd("CDUP")
    a.cmd("PWD")
    a.send_file("STOR hack.txt", b"nope\n")
    a.cmd("DELE readme.txt")
    a.cmd("TYPE X")
    a.cmd("HELP")

    print("== session 2: dev uploads while anonymous stays connected")
    d = Client("dev")
    d.cmd("USER dev")
    d.cmd("PASS wrong")
    d.cmd("PASS s3cret")
    d.cmd("USER dev")
    d.cmd("PASS s3cret")
    d.cmd("CWD /incoming")
    d.send_file("STOR report.txt", b"quarter one\r\nquarter two\r\n")
    d.cmd("TYPE I")
    d.send_file("STOR raw.bin", b"a\r\nb\n", extended=True)
    d.cmd("SIZE report.txt")
    d.cmd("SIZE raw.bin")
    d.cmd("TYPE A")
    d.send_file("APPE report.txt", b"quarter three\r\n")
    d.fetch("RETR report.txt")
    d.cmd("MKD archive")
    d.cmd("MKD archive")
    d.cmd("MKD /nowhere/deep")
    d.cmd("RNTO elsewhere.txt")
    d.cmd("RNFR report.txt")
    d.cmd("RNTO archive/q.txt")
    d.cmd("RNFR ghost.txt")
    d.cmd("RMD archive")
    d.fetch("LIST archive", extended=True)
    d.fetch("LIST /")
    d.send_file("STOR /nowhere/x.txt", b"x\n")

    a.fetch("NLST /incoming")
    a.cmd("NOOP")
    a.cmd("QUIT")
    a.expect_close()
    a.close()

    print("== session 3: dev tidies up")
    e = Client("dev2")
    e.cmd("PASS s3cret")
    e.cmd("USER dev")
    e.cmd("PASS s3cret")
    e.cmd("DELE /incoming/archive/q.txt")
    e.cmd("DELE /incoming/archive/q.txt")
    e.cmd("RMD /incoming/archive")
    e.cmd("RMD /")
    e.cmd("PASV")
    e.cmd("QUIT")
    e.expect_close()
    e.close()

    d.cmd("QUIT")
    d.expect_close()
    d.close()

    server.join()
    print("== server state")
    for path in sorted(list(st.files) + list(st.dirs)):
        if path == "/":
            print("/")
        elif path in st.dirs:
            print(f"{path}/")
        else:
            print(f"{path} ({len(st.files[path])} bytes)")
    for verb, n in sorted(st.counts.items()):
        print(f"{verb} {n}")
    listener.close()


main()
