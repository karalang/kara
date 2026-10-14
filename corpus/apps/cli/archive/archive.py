# Python mirror of source.kara: same format, same members, same output.
import struct
import zlib

class Bad(Exception):
    pass

def pack(members):
    out = bytearray(b"KAR1") + struct.pack("<I", len(members))
    for name, data in members:
        nb = name.encode()
        out += struct.pack("<H", len(nb)) + nb + struct.pack("<II", len(data), zlib.crc32(data)) + data
    return bytes(out)

def unpack(b):
    at = 0
    def take(n):
        nonlocal at
        if at + n > len(b):
            raise Bad(f"truncated at byte {at}: wanted {n}, have {len(b) - at}")
        s = b[at:at + n]
        at += n
        return s
    if take(4) != b"KAR1":
        raise Bad("not a KAR1 archive")
    count = struct.unpack("<I", take(4))[0]
    out = []
    for i in range(count):
        n = struct.unpack("<H", take(2))[0]
        try:
            name = take(n).decode()
        except UnicodeDecodeError:
            raise Bad(f"member {i}: name is not UTF-8")
        size = struct.unpack("<I", take(4))[0]
        want = struct.unpack("<I", take(4))[0]
        data = take(size)
        got = zlib.crc32(data)
        if got != want:
            raise Bad(f"{name}: crc {got:08x} != stored {want:08x}")
        out.append((name, data, got))
    if at != len(b):
        raise Bad(f"{len(b) - at} trailing bytes")
    return out

def main():
    print(f'crc32("123456789") = {zlib.crc32(b"123456789"):08x}')
    binary = bytes(((i * 37 + 11) % 256) for i in range(300))
    members = [
        ("README.md", b"# demo\nA small archive.\n"),
        ("src/main.kara", b'fn main() {\n    println("hi");\n}\n'),
        ("empty.txt", b""),
        ("notes/ünïcode.txt", "naïve café\n".encode()),
        ("blob.bin", binary),
    ]
    archive = pack(members)
    open("demo.kar", "wb").write(archive)
    print(f"wrote demo.kar: {len(archive)} bytes")
    data = open("demo.kar", "rb").read()
    ms = unpack(data)
    for name, d, crc in ms:
        print(f"  {name:<20} {len(d):>5} bytes  crc {crc:08x}")
    for name, d, _ in ms:
        open("x_" + name.replace("/", "_"), "wb").write(d)
    print(f"extracted {len(ms)}")
    print(f"x_src_main.kara line 2: [{open('x_src_main.kara').read().splitlines()[1]}]")
    c = bytearray(data)
    c[len(c) - 100] ^= 0x40
    for label, blob in [("corrupted", bytes(c)), ("truncated", data[:60]), ("padded", data + b"\0\0"),
                        ("zip", b"PK\x03\x04junk")]:
        try:
            unpack(blob)
            print(f"{label} not detected")
        except Bad as e:
            print(f"{label}: {e}")

main()
