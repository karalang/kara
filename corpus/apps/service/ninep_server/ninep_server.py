# Oracle for ninep_server.kara: a 9P2000 file server (the Plan 9 file
# protocol, also v9fs in Linux and the protocol under WSL's and QEMU's file
# sharing) over TCP, with an in-memory tree, and the clients that use it.
# Messages are little-endian: size[4] type[1] tag[2], then the body.
# Run: python3 -I -B ninep_server.py > expected.out
import socket
import struct
import threading

NOFID = 0xFFFFFFFF
NOTAG = 0xFFFF
MAX_MSIZE = 8192
QTDIR = 0x80
DMDIR = 0x80000000
BASE_TIME = 1700000000
OREAD, OWRITE, ORDWR, OTRUNC = 0, 1, 2, 0x10
IOHDR = 24  # size, type, tag, fid, offset, count: the most a header takes

NAMES = {
    100: "Tversion", 101: "Rversion", 102: "Tauth", 103: "Rauth",
    104: "Tattach", 105: "Rattach", 107: "Rerror", 108: "Tflush",
    109: "Rflush", 110: "Twalk", 111: "Rwalk", 112: "Topen", 113: "Ropen",
    114: "Tcreate", 115: "Rcreate", 116: "Tread", 117: "Rread",
    118: "Twrite", 119: "Rwrite", 120: "Tclunk", 121: "Rclunk",
    122: "Tremove", 123: "Rremove", 124: "Tstat", 125: "Rstat",
}


# ------------------------------------------------------------ wire format

class W:
    def __init__(self):
        self.b = bytearray()

    def u8(self, v):
        self.b += struct.pack("<B", v)
        return self

    def u16(self, v):
        self.b += struct.pack("<H", v)
        return self

    def u32(self, v):
        self.b += struct.pack("<I", v)
        return self

    def u64(self, v):
        self.b += struct.pack("<Q", v)
        return self

    def s(self, text):
        raw = text.encode()
        self.u16(len(raw))
        self.b += raw
        return self

    def raw(self, data):
        self.b += data
        return self

    def qid(self, q):
        qtype, vers, path = q
        return self.u8(qtype).u32(vers).u64(path)


class R:
    def __init__(self, data):
        self.d = data
        self.at = 0

    def take(self, n):
        if self.at + n > len(self.d):
            raise ValueError("short message")
        out = self.d[self.at:self.at + n]
        self.at += n
        return out

    def u8(self):
        return self.take(1)[0]

    def u16(self):
        return struct.unpack("<H", self.take(2))[0]

    def u32(self):
        return struct.unpack("<I", self.take(4))[0]

    def u64(self):
        return struct.unpack("<Q", self.take(8))[0]

    def s(self):
        return self.take(self.u16()).decode()

    def qid(self):
        return (self.u8(), self.u32(), self.u64())


def frame(mtype, tag, body):
    return struct.pack("<IBH", 7 + len(body), mtype, tag) + bytes(body)


def read_frame(sock_file):
    head = sock_file.read(4)
    if len(head) < 4:
        return None
    size = struct.unpack("<I", head)[0]
    rest = sock_file.read(size - 4)
    if len(rest) < size - 4:
        return None
    return rest[0], struct.unpack("<H", rest[1:3])[0], rest[3:]


def show_qid(q):
    qtype, vers, path = q
    return f"qid({path} v{vers} {'dir' if qtype & QTDIR else 'file'})"


def escape(data):
    return data.decode().replace("\n", "\\n").replace("\x00", "\\0")


# ------------------------------------------------------------ the tree

class Node:
    def __init__(self, path, name, is_dir, perm, uid, parent, now):
        self.path = path
        self.name = name
        self.is_dir = is_dir
        self.perm = perm
        self.uid = uid
        self.gid = uid
        self.muid = uid
        self.parent = parent if parent is not None else self
        self.children = []
        self.data = bytearray()
        self.version = 0
        self.atime = now
        self.mtime = now
        self.ctl = False

    def qid(self):
        return (QTDIR if self.is_dir else 0, self.version, self.path)

    def stat(self):
        body = (W().u16(0).u32(0).qid(self.qid())
                .u32(self.perm | (DMDIR if self.is_dir else 0))
                .u32(BASE_TIME + self.atime).u32(BASE_TIME + self.mtime)
                .u64(0 if self.is_dir else len(self.data))
                .s(self.name).s(self.uid).s(self.gid).s(self.muid)).b
        return W().u16(len(body)).raw(body).b

    def child(self, name):
        for c in self.children:
            if c.name == name:
                return c
        return None


class NineError(Exception):
    pass


class Server:
    def __init__(self):
        self.lock = threading.Lock()
        self.clock = 0
        self.next_path = 1
        self.root = self.make(None, "/", True, 0o775, "admin")
        readme = self.make(self.root, "readme", False, 0o644, "admin")
        readme.data += b"served by ninep_server\n"
        etc = self.make(self.root, "etc", True, 0o755, "glenda")
        motd = self.make(etc, "motd", False, 0o644, "glenda")
        motd.data += b"welcome to the grid\n"
        self.make(self.root, "tmp", True, 0o777, "admin")
        ctl = self.make(self.root, "ctl", False, 0o200, "admin")
        ctl.ctl = True
        self.stopping = False
        self.log = []
        self.counts = {}

    def make(self, parent, name, is_dir, perm, uid):
        n = Node(self.next_path, name, is_dir, perm, uid, parent, self.clock)
        self.next_path += 1
        if parent is not None:
            parent.children.append(n)
        return n

    def walk_path(self, n):
        parts = []
        while n.parent is not n:
            parts.append(n.name)
            n = n.parent
        return "/" + "/".join(reversed(parts))


def allowed(node, user, want):
    bits = (node.perm >> 6) & 7 if node.uid == user else node.perm & 7
    return bits & want == want


class Fid:
    def __init__(self, node):
        self.node = node
        self.mode = None  # None until opened
        self.dir_offset = 0
        self.dir_index = 0


class Session:
    def __init__(self, server, conn_no):
        self.srv = server
        self.conn_no = conn_no
        self.msize = 0
        self.user = None
        self.fids = {}
        self.count = 0

    def fid(self, f):
        if f not in self.fids:
            raise NineError("unknown fid")
        return self.fids[f]

    def handle(self, mtype, tag, body):
        """One T-message; returns (R type, body)."""
        self.count += 1
        srv = self.srv
        with srv.lock:
            srv.clock += 1
            try:
                return self.dispatch(mtype, R(body))
            except NineError as e:
                return 107, W().s(str(e)).b
            except ValueError:
                return 107, W().s("malformed message").b

    def dispatch(self, mtype, r):
        srv = self.srv
        if mtype == 100:
            msize, version = r.u32(), r.s()
            self.fids.clear()
            self.msize = min(msize, MAX_MSIZE)
            if not version.startswith("9P2000"):
                return 101, W().u32(self.msize).s("unknown").b
            return 101, W().u32(self.msize).s("9P2000").b
        if self.msize == 0:
            raise NineError("version not negotiated")
        if mtype == 102:
            raise NineError("authentication not required")
        if mtype == 104:
            fid, afid, uname, aname = r.u32(), r.u32(), r.s(), r.s()
            if afid != NOFID:
                raise NineError("authentication not required")
            if aname not in ("", "/"):
                raise NineError(f"no such tree: {aname}")
            if fid in self.fids:
                raise NineError("fid already in use")
            self.user = uname
            self.fids[fid] = Fid(srv.root)
            srv.log.append(f"conn {self.conn_no}: {uname} attached")
            return 105, W().qid(srv.root.qid()).b
        if mtype == 108:
            r.u16()
            return 109, b""
        if mtype == 110:
            fid, newfid, n = r.u32(), r.u32(), r.u16()
            names = [r.s() for _ in range(n)]
            f = self.fid(fid)
            if f.mode is not None:
                raise NineError("cannot walk an open fid")
            if newfid != fid and newfid in self.fids:
                raise NineError("fid already in use")
            if n > 16:
                raise NineError("too many names in walk")
            node = f.node
            qids = []
            for name in names:
                if not node.is_dir:
                    if not qids:
                        raise NineError("not a directory")
                    break
                nxt = node.parent if name == ".." else node.child(name)
                if nxt is None:
                    if not qids:
                        raise NineError("file does not exist")
                    break
                node = nxt
                qids.append(node.qid())
            if len(qids) == n:
                self.fids[newfid] = Fid(node)
            w = W().u16(len(qids))
            for q in qids:
                w.qid(q)
            return 111, w.b
        if mtype == 112:
            fid, mode = r.u32(), r.u8()
            f = self.fid(fid)
            if f.mode is not None:
                raise NineError("fid already open")
            self.check_open(f.node, mode)
            if mode & OTRUNC:
                f.node.data = bytearray()
                self.touch(f.node)
            f.mode = mode
            return 113, W().qid(f.node.qid()).u32(self.msize - IOHDR).b
        if mtype == 114:
            fid, name, perm, mode = r.u32(), r.s(), r.u32(), r.u8()
            f = self.fid(fid)
            d = f.node
            if f.mode is not None:
                raise NineError("fid already open")
            if not d.is_dir:
                raise NineError("not a directory")
            if name in ("", ".", "..") or "/" in name:
                raise NineError("bad file name")
            if d.child(name) is not None:
                raise NineError("file exists")
            if not allowed(d, self.user, 2):
                raise NineError("permission denied")
            is_dir = bool(perm & DMDIR)
            if is_dir and mode & 3 != OREAD:
                raise NineError("is a directory")
            n = srv.make(d, name, is_dir, perm & 0o777, self.user)
            n.atime = n.mtime = srv.clock
            self.touch(d)
            f.node = n
            f.mode = mode
            srv.log.append(f"conn {self.conn_no}: {self.user} created {srv.walk_path(n)}")
            return 115, W().qid(n.qid()).u32(self.msize - IOHDR).b
        if mtype == 116:
            fid, offset, count = r.u32(), r.u64(), r.u32()
            f = self.fid(fid)
            if f.mode is None or f.mode & 3 not in (OREAD, ORDWR):
                raise NineError("fid not open for reading")
            count = min(count, self.msize - IOHDR)
            n = f.node
            n.atime = srv.clock
            if not n.is_dir:
                data = bytes(n.data[offset:offset + count])
                return 117, W().u32(len(data)).raw(data).b
            if offset == 0:
                f.dir_index = 0
                f.dir_offset = 0
            elif offset != f.dir_offset:
                raise NineError("bad offset in directory read")
            out = bytearray()
            while f.dir_index < len(n.children):
                st = n.children[f.dir_index].stat()
                if len(out) + len(st) > count:
                    break
                out += st
                f.dir_index += 1
            if not out and f.dir_index < len(n.children):
                raise NineError("count too small for a directory entry")
            f.dir_offset += len(out)
            return 117, W().u32(len(out)).raw(out).b
        if mtype == 118:
            fid, offset, count = r.u32(), r.u64(), r.u32()
            data = r.take(count)
            f = self.fid(fid)
            if f.mode is None or f.mode & 3 not in (OWRITE, ORDWR):
                raise NineError("fid not open for writing")
            n = f.node
            if n.ctl:
                cmd = data.decode().strip()
                if cmd != "halt":
                    raise NineError(f"unknown ctl message: {cmd}")
                srv.stopping = True
                srv.log.append(f"conn {self.conn_no}: {self.user} wrote halt to /ctl")
                return 119, W().u32(count).b
            if offset > len(n.data):
                n.data += bytes(offset - len(n.data))
            n.data[offset:offset + count] = data
            self.touch(n)
            return 119, W().u32(count).b
        if mtype == 120:
            fid = r.u32()
            self.fid(fid)
            del self.fids[fid]
            return 121, b""
        if mtype == 122:
            fid = r.u32()
            f = self.fid(fid)
            del self.fids[fid]  # a remove clunks the fid, whatever happens
            n = f.node
            if n is srv.root:
                raise NineError("cannot remove the root")
            if n.is_dir and n.children:
                raise NineError("directory not empty")
            if not allowed(n.parent, self.user, 2):
                raise NineError("permission denied")
            where = srv.walk_path(n)
            n.parent.children.remove(n)
            self.touch(n.parent)
            srv.log.append(f"conn {self.conn_no}: {self.user} removed {where}")
            return 123, b""
        if mtype == 124:
            fid = r.u32()
            st = self.fid(fid).node.stat()
            return 125, W().u16(len(st)).raw(st).b
        raise NineError(f"unknown message type {mtype}")

    def check_open(self, node, mode):
        if node.is_dir and (mode & 3 != OREAD or mode & OTRUNC):
            raise NineError("is a directory")
        want = {OREAD: 4, OWRITE: 2, ORDWR: 6, 3: 1}[mode & 3]
        if mode & OTRUNC:
            want |= 2
        if not allowed(node, self.user, want):
            raise NineError("permission denied")

    def touch(self, n):
        n.version += 1
        n.mtime = self.srv.clock
        n.muid = self.user


def serve_conn(srv, sock, conn_no):
    f = sock.makefile("rb")
    sess = Session(srv, conn_no)
    while True:
        msg = read_frame(f)
        if msg is None:
            break
        mtype, tag, body = msg
        rtype, rbody = sess.handle(mtype, tag, body)
        sock.sendall(frame(rtype, tag, rbody))
    f.close()
    sock.close()
    with srv.lock:
        srv.counts[conn_no] = sess.count


def serve(srv, listener):
    workers = []
    conn_no = 0
    while True:
        sock, _ = listener.accept()
        with srv.lock:
            stop = srv.stopping
        if stop:
            sock.close()
            break
        conn_no += 1
        t = threading.Thread(target=serve_conn, args=(srv, sock, conn_no))
        t.start()
        workers.append(t)
    listener.close()
    for t in workers:
        t.join()


# ------------------------------------------------------------ the clients

class Client:
    def __init__(self, name, addr):
        self.name = name
        self.sock = socket.create_connection(addr)
        self.f = self.sock.makefile("rb")
        self.next_tag = 1
        self.dirs = set()  # fids that name directories

    def tag(self):
        t = self.next_tag
        self.next_tag += 1
        return t

    def send(self, mtype, body, tag=None, show=""):
        tag = self.tag() if tag is None else tag
        print(f"{self.name} -> {NAMES.get(mtype, f'T{mtype}')} tag={tag} {show}".rstrip())
        self.sock.sendall(frame(mtype, tag, body))
        return tag

    def recv(self, context=None):
        mtype, tag, body = read_frame(self.f)
        r = R(body)
        self.got = 0
        name = NAMES[mtype]
        if mtype == 107:
            text = f"'{r.s()}'"
        elif mtype == 101:
            text = f"msize={r.u32()} version={r.s()}"
        elif mtype in (105, 113, 115):
            text = show_qid(r.qid())
            if mtype != 105:
                text += f" iounit={r.u32()}"
        elif mtype == 111:
            text = "[" + ", ".join(show_qid(r.qid()) for _ in range(r.u16())) + "]"
        elif mtype == 117:
            data = r.take(r.u32())
            self.got = len(data)
            if context == "dir":
                rd = R(data)
                entries = []
                while rd.at < len(data):
                    entries.append(show_stat(R(rd.take(rd.u16()))))
                text = f"{len(data)} bytes, {len(entries)} entries" + "".join(
                    f"\n{self.name}      | {e}" for e in entries)
            else:
                text = f"{len(data)} bytes '{escape(data)}'"
        elif mtype == 119:
            text = f"count={r.u32()}"
        elif mtype == 125:
            st = R(r.take(r.u16()))
            st.u16()  # the stat's own size
            text = show_stat(st)
        else:
            text = ""
        print(f"{self.name} <- {name} tag={tag} {text}".rstrip())
        return mtype, tag

    def version(self, msize, version="9P2000", hexdump=False):
        body = W().u32(msize).s(version).b
        if hexdump:
            print(f"{self.name}    bytes: {frame(100, NOTAG, body).hex(' ')}")
        self.send(100, body, NOTAG, f"msize={msize} version={version}")
        return self.recv()

    def attach(self, fid, uname, aname=""):
        self.send(104, W().u32(fid).u32(NOFID).s(uname).s(aname).b, show=f"fid={fid} uname={uname} aname='{aname}'")
        return self.recv()

    def walk(self, fid, newfid, names):
        w = W().u32(fid).u32(newfid).u16(len(names))
        for n in names:
            w.s(n)
        self.send(110, w.b, show=f"fid={fid} newfid={newfid} [{', '.join(names)}]")
        return self.recv()

    def open(self, fid, mode):
        self.send(112, W().u32(fid).u8(mode).b, show=f"fid={fid} mode={mode_name(mode)}")
        mtype, _ = self.recv()
        return mtype

    def create(self, fid, name, perm, mode):
        self.send(114, W().u32(fid).s(name).u32(perm).u8(mode).b,
                  show=f"fid={fid} name={name} perm={perm_name(perm)} mode={mode_name(mode)}")
        return self.recv()

    def read(self, fid, offset, count):
        self.send(116, W().u32(fid).u64(offset).u32(count).b, show=f"fid={fid} offset={offset} count={count}")
        self.recv("dir" if fid in self.dirs else None)
        return self.got

    def write(self, fid, offset, data):
        self.send(118, W().u32(fid).u64(offset).u32(len(data)).raw(data).b,
                  show=f"fid={fid} offset={offset} '{escape(data)}'")
        return self.recv()

    def clunk(self, fid):
        self.send(120, W().u32(fid).b, show=f"fid={fid}")
        self.dirs.discard(fid)
        return self.recv()

    def remove(self, fid):
        self.send(122, W().u32(fid).b, show=f"fid={fid}")
        self.dirs.discard(fid)
        return self.recv()

    def stat(self, fid):
        self.send(124, W().u32(fid).b, show=f"fid={fid}")
        return self.recv()

    def close(self):
        self.f.close()
        self.sock.close()


def show_stat(r):
    r.u16()
    r.u32()
    q = r.qid()
    mode = r.u32()
    atime, mtime, length = r.u32(), r.u32(), r.u64()
    name, uid, gid, muid = r.s(), r.s(), r.s(), r.s()
    kind = "d" if mode & DMDIR else "-"
    return (f"{kind}{oct(mode & 0o777)[2:]:0>3} {name} len={length} uid={uid} gid={gid} muid={muid} "
            f"{show_qid(q)} atime=+{atime - BASE_TIME} mtime=+{mtime - BASE_TIME}")


def mode_name(m):
    base = ["OREAD", "OWRITE", "ORDWR", "OEXEC"][m & 3]
    return base + ("|OTRUNC" if m & OTRUNC else "")


def perm_name(p):
    return ("DMDIR|" if p & DMDIR else "") + oct(p & 0o777)[2:].rjust(3, "0")


def main():
    srv = Server()
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(8)
    addr = listener.getsockname()
    server = threading.Thread(target=serve, args=(srv, listener))
    server.start()

    print("== alice connects")
    a = Client("alice", addr)
    a.attach(0, "alice")
    a.version(16384, hexdump=True)
    a.attach(0, "alice", "archive")
    a.attach(0, "alice")
    a.walk(0, 1, ["etc", "motd"])
    a.open(1, OREAD)
    a.read(1, 0, 100)
    a.read(1, 11, 4)
    a.stat(1)
    a.walk(0, 2, ["nope"])
    a.walk(0, 2, ["etc", "nope", "deeper"])
    a.clunk(2)
    a.walk(0, 2, ["etc", "motd", "x"])
    a.walk(0, 2, [])
    a.open(2, OWRITE)
    a.dirs.add(2)
    a.open(2, OREAD)
    a.read(2, 0, 8192)
    first = a.read(2, 0, 200)
    a.read(2, first + 1, 200)
    second = a.read(2, first, 200)
    a.read(2, first + second, 200)
    a.read(2, 0, 40)
    a.walk(1, 3, [])
    a.walk(0, 1, [])
    a.walk(0, 3, ["etc", "..", "tmp"])
    a.create(3, "notes", 0o666, ORDWR)
    a.write(3, 0, b"first line\n")
    a.write(3, 11, b"second line\n")
    a.read(3, 0, 8192)
    a.create(3, "more", 0o644, OREAD)

    print("== bob connects")
    b = Client("bob", addr)
    b.version(8192, "9P2000.u")
    b.attach(10, "bob")
    b.walk(10, 11, ["etc", "motd"])
    b.open(11, OWRITE)
    b.open(11, OREAD)
    b.walk(10, 12, ["tmp", "notes"])
    b.open(12, OWRITE | OTRUNC)
    b.write(12, 0, b"bob was here\n")
    b.write(12, 20, b"!")
    b.walk(10, 13, ["ctl"])
    b.open(13, OWRITE)
    b.walk(10, 14, ["tmp"])
    b.create(14, "box", DMDIR | 0o755, OREAD)
    b.walk(10, 15, ["readme"])
    b.remove(15)
    b.stat(15)

    print("== alice again")
    a.read(3, 0, 8192)
    a.stat(3)
    print("== alice pipelines three stats")
    tags = []
    for fid in (0, 1, 3):
        tags.append(a.send(124, W().u32(fid).b, show=f"fid={fid}"))
    for _ in tags:
        a.recv()
    a.send(99, b"\x00\x00", show="(a type the protocol does not have)")
    a.recv()
    a.send(102, W().u32(5).s("alice").s("").b, show="afid=5 uname=alice")
    a.recv()
    a.send(108, W().u16(4).b, show="oldtag=4")
    a.recv()
    a.walk(0, 4, ["tmp"])
    a.remove(4)
    a.clunk(4)
    a.walk(0, 4, ["tmp", "box"])
    a.remove(4)
    a.remove(3)
    a.walk(0, 5, ["tmp", "notes"])
    a.clunk(1)
    a.clunk(1)

    print("== admin halts the server")
    c = Client("admin", addr)
    c.version(8192)
    c.attach(0, "admin")
    c.walk(0, 1, ["ctl"])
    c.open(1, OWRITE)
    c.write(1, 0, b"reboot\n")
    c.write(1, 0, b"halt\n")
    a.close()
    b.close()
    c.close()
    socket.create_connection(addr).close()  # wakes the accept loop
    server.join()

    print("== server log")
    for line in srv.log:
        print(line)
    for conn_no in sorted(srv.counts):
        print(f"conn {conn_no}: closed after {srv.counts[conn_no]} messages")
    print("== final tree")
    show_tree(srv.root, 0)


def show_tree(n, depth):
    kind = "d" if n.is_dir else "-"
    size = "" if n.is_dir else f" {len(n.data)} bytes"
    print(f"{'  ' * depth}{kind}{oct(n.perm)[2:]:0>3} {n.name} path={n.path} v{n.version} "
          f"uid={n.uid} muid={n.muid}{size}")
    for c in n.children:
        show_tree(c, depth + 1)


main()
