# pipeline/thumb_render, the Python mirror. It runs the same pipeline as
# thumb_render.kara with the same integer arithmetic and wrote expected.out.
import os
import sys

W = 160
H = 120
FP_SHIFT = 20
FP = 1 << FP_SHIFT
MAXIT = 96


def escape(cx, cy, zr, zi):
    i = 0
    while i < MAXIT:
        zr2 = (zr * zr) >> FP_SHIFT
        zi2 = (zi * zi) >> FP_SHIFT
        if zr2 + zi2 > 4 * FP:
            break
        zi = ((2 * zr * zi) >> FP_SHIFT) + cy
        zr = zr2 - zi2 + cx
        i += 1
    return i


def render_mandel():
    px = []
    for y in range(H):
        cy = -1258291 + y * 20971
        for x in range(W):
            cx = -2306867 + x * 20971
            i = escape(cx, cy, 0, 0)
            if i == MAXIT:
                px += [0, 0, 0]
            else:
                px += [(i * 7) % 256, (i * 13 + 60) % 256, 255 - (i * 5) % 256]
    return px


def render_julia():
    px = []
    for y in range(H):
        zi = -1258291 + y * 20971
        for x in range(W):
            zr = -1677721 + x * 20971
            i = escape(-838861, 163578, zr, zi)
            if i == MAXIT:
                px += [10, 0, 30]
            else:
                px += [(i * 11) % 256, (i * 3) % 256, (i * 17 + 100) % 256]
    return px


def sine_table():
    t = []
    for k in range(256):
        deg = k * 360 // 256
        neg = deg > 180
        d = deg - 180 if neg else deg
        num = 4 * d * (180 - d)
        den = 40500 - d * (180 - d)
        s = num * 127 // den
        t.append(-s if neg else s)
    return t


def isqrt(n):
    if n < 2:
        return n
    x = n
    y = (x + 1) // 2
    while y < x:
        x = y
        y = (x + n // x) // 2
    return x


def render_plasma(t):
    px = []
    for y in range(H):
        for x in range(W):
            dx = x - 80
            dy = y - 60
            v = (t[(x * 3) % 256] + t[(y * 5) % 256] + t[((x + y) * 2) % 256]
                 + t[(isqrt(dx * dx + dy * dy) * 4) % 256])
            idx = (v + 512) // 4
            px += [t[idx % 256] + 128, t[(idx + 85) % 256] + 128, t[(idx + 170) % 256] + 128]
    return px


M32 = 0xFFFFFFFF


def lattice(ix, iy, seed):
    h = (ix * 374761393 + iy * 668265263 + seed * 2147483647) & M32
    h = ((h ^ (h >> 13)) * 461845907) & M32
    h = h ^ (h >> 16)
    return h & 255


def noise(x, y, cell, seed):
    ix = x // cell
    iy = y // cell
    fx = x % cell
    fy = y % cell
    top = lattice(ix, iy, seed) * (cell - fx) + lattice(ix + 1, iy, seed) * fx
    bot = lattice(ix, iy + 1, seed) * (cell - fx) + lattice(ix + 1, iy + 1, seed) * fx
    return (top * (cell - fy) + bot * fy) // (cell * cell)


def render_terrain():
    px = []
    for y in range(H):
        for x in range(W):
            h = (noise(x, y, 32, 1) * 4 + noise(x, y, 16, 2) * 2 + noise(x, y, 8, 3)) // 7
            if h < 90:
                px += [20, 40 + h, 120 + h]
            elif h < 105:
                px += [194, 178, 128]
            elif h < 160:
                px += [34 + (h - 105), 139 - (h - 105) // 2, 34]
            elif h < 210:
                g = 110 + (h - 160) // 2
                px += [g, g - 10, g - 20]
            else:
                s = 240 + (h - 210) // 6
                px += [s, s, s]
    return px


def encode_ppm(px, w, h):
    return f"P6\n{w} {h}\n255\n".encode() + bytes(px)


class DecodeError(Exception):
    pass


def decode_ppm(name, data):
    # Header: magic, width, height, maxval, separated by whitespace, then
    # exactly one whitespace byte before the pixels.
    fields = []
    pos = 0
    while len(fields) < 4:
        while pos < len(data) and data[pos] in b" \t\r\n":
            pos += 1
        start = pos
        while pos < len(data) and data[pos] not in b" \t\r\n":
            pos += 1
        if start == pos:
            raise DecodeError(f"{name}: header ends after {len(fields)} fields")
        fields.append(data[start:pos].decode("latin-1"))
    if fields[0] != "P6":
        raise DecodeError(f"{name}: magic is {fields[0]}, not P6")
    for f in fields[1:]:
        if not f.isdigit():
            raise DecodeError(f"{name}: {f} is not a number")
    w, h, maxval = int(fields[1]), int(fields[2]), int(fields[3])
    if maxval != 255:
        raise DecodeError(f"{name}: maxval {maxval} is not 255")
    pos += 1
    need = w * h * 3
    have = len(data) - pos
    if have != need:
        raise DecodeError(f"{name}: {have} pixel bytes, {w}x{h} needs {need}")
    return (w, h, list(data[pos:]))


def blur(px, w, h):
    k = [1, 2, 1]
    out = [0] * (w * h * 3)
    for y in range(h):
        for x in range(w):
            for c in range(3):
                s = 0
                for dy in range(3):
                    yy = min(max(y + dy - 1, 0), h - 1)
                    for dx in range(3):
                        xx = min(max(x + dx - 1, 0), w - 1)
                        s += px[(yy * w + xx) * 3 + c] * k[dy] * k[dx]
                out[(y * w + x) * 3 + c] = (s + 8) // 16
    return out


def make_thumb(img):
    w, h, px = img
    b = blur(px, w, h)
    tw = w // 4
    th = h // 4
    out = []
    for ty in range(th):
        for tx in range(tw):
            for c in range(3):
                s = 0
                for yy in range(4):
                    for xx in range(4):
                        s += b[((ty * 4 + yy) * w + tx * 4 + xx) * 3 + c]
                out.append((s + 8) // 16)
    return (tw, th, out)


def luma(r, g, b):
    return (77 * r + 150 * g + 29 * b) >> 8


def histogram(name, img):
    w, h, px = img
    buckets = [0] * 8
    for i in range(w * h):
        buckets[luma(px[i * 3], px[i * 3 + 1], px[i * 3 + 2]) >> 5] += 1
    return f"{name} luma histogram: " + " ".join(str(b) for b in buckets)


def channel_stats(name, img):
    w, h, px = img
    n = w * h
    parts = []
    for c, label in enumerate("rgb"):
        s = 0
        sq = 0
        for i in range(n):
            v = px[i * 3 + c]
            s += v
            sq += v * v
        mean = s // n
        std = isqrt((sq * n - s * s) // (n * n))
        parts.append(f"{label} {mean}+-{std}")
    return f"{name} channels: " + ", ".join(parts)


def edge_count(name, img):
    w, h, px = img
    lum = [luma(px[i * 3], px[i * 3 + 1], px[i * 3 + 2]) for i in range(w * h)]
    edges = 0
    strongest = 0
    for y in range(1, h - 1):
        for x in range(1, w - 1):
            def at(dx, dy):
                return lum[(y + dy) * w + x + dx]
            gx = (at(1, -1) + 2 * at(1, 0) + at(1, 1)) - (at(-1, -1) + 2 * at(-1, 0) + at(-1, 1))
            gy = (at(-1, 1) + 2 * at(0, 1) + at(1, 1)) - (at(-1, -1) + 2 * at(0, -1) + at(1, -1))
            mag = abs(gx) + abs(gy)
            if mag > 128:
                edges += 1
            if mag > strongest:
                strongest = mag
    return f"{name} edges: {edges} of {(w - 2) * (h - 2)}, strongest {strongest}"


def palette(name, thumb):
    w, h, px = thumb
    boxes = [[(px[i * 3], px[i * 3 + 1], px[i * 3 + 2]) for i in range(w * h)]]
    while len(boxes) < 4:
        best = -1
        best_range = 0
        best_c = 0
        for bi, box in enumerate(boxes):
            for c in range(3):
                lo = min(p[c] for p in box)
                hi = max(p[c] for p in box)
                if hi - lo > best_range:
                    best = bi
                    best_range = hi - lo
                    best_c = c
        if best < 0:
            break
        box = boxes[best]
        c = best_c
        box.sort(key=lambda p: (p[c], p[0], p[1], p[2]))
        mid = len(box) // 2
        boxes[best] = box[:mid]
        boxes.insert(best + 1, box[mid:])
    colors = []
    for box in boxes:
        n = len(box)
        colors.append((sum(p[0] for p in box) // n, sum(p[1] for p in box) // n,
                       sum(p[2] for p in box) // n, n))
    colors.sort(key=lambda q: (luma(q[0], q[1], q[2]), q[0], q[1], q[2], q[3]))
    return f"{name} palette: " + " ".join(f"#{r:02x}{g:02x}{b:02x}x{n}" for r, g, b, n in colors)


RAMP = " .:-=+*#%@"


def preview(name, thumb):
    w, h, px = thumb
    lines = [f"{name} preview:"]
    for y in range(0, h, 2):
        row = ""
        for x in range(w):
            i = (y * w + x) * 3
            row += RAMP[luma(px[i], px[i + 1], px[i + 2]) * 10 // 256]
        lines.append("  |" + row + "|")
    return "\n".join(lines)


def crc_table():
    t = []
    for n in range(256):
        c = n
        for _ in range(8):
            if c & 1:
                c = 0xEDB88320 ^ (c >> 1)
            else:
                c = c >> 1
        t.append(c)
    return t


def crc32(t, data, start, end):
    c = 0xFFFFFFFF
    for i in range(start, end):
        c = t[(c ^ data[i]) & 255] ^ (c >> 8)
    return c ^ 0xFFFFFFFF


def adler32(data):
    a = 1
    b = 0
    for v in data:
        a = (a + v) % 65521
        b = (b + a) % 65521
    return b * 65536 + a


def be32(v):
    return [(v >> 24) & 255, (v >> 16) & 255, (v >> 8) & 255, v & 255]


def chunk(t, kind, body):
    data = list(kind.encode()) + body
    return be32(len(body)) + data + be32(crc32(t, data, 0, len(data)))


def encode_png(t, w, h, px):
    raw = []
    for y in range(h):
        raw.append(0)
        raw += px[y * w * 3:(y + 1) * w * 3]
    z = [0x78, 0x01]
    pos = 0
    while True:
        n = min(len(raw) - pos, 65535)
        final = 1 if pos + n == len(raw) else 0
        z += [final, n & 255, n >> 8, (65535 - n) & 255, (65535 - n) >> 8]
        z += raw[pos:pos + n]
        pos += n
        if final:
            break
    z += be32(adler32(raw))
    ihdr = be32(w) + be32(h) + [8, 2, 0, 0, 0]
    out = [137, 80, 78, 71, 13, 10, 26, 10]
    out += chunk(t, "IHDR", ihdr)
    out += chunk(t, "IDAT", z)
    out += chunk(t, "IEND", [])
    return out


def rd32(d, i):
    return (d[i] << 24) | (d[i + 1] << 16) | (d[i + 2] << 8) | d[i + 3]


def decode_png(t, d):
    if d[:8] != [137, 80, 78, 71, 13, 10, 26, 10]:
        raise DecodeError("png: bad signature")
    pos = 8
    w = h = 0
    z = []
    kinds = []
    while pos < len(d):
        n = rd32(d, pos)
        kind = bytes(d[pos + 4:pos + 8]).decode("latin-1")
        crc = rd32(d, pos + 8 + n)
        if crc32(t, d, pos + 4, pos + 8 + n) != crc:
            raise DecodeError(f"png: {kind} crc mismatch")
        body = d[pos + 8:pos + 8 + n]
        kinds.append(f"{kind}({n})")
        if kind == "IHDR":
            w = rd32(body, 0)
            h = rd32(body, 4)
        elif kind == "IDAT":
            z += body
        pos += 12 + n
    raw = []
    i = 2
    while True:
        final = z[i] & 1
        n = z[i + 1] | (z[i + 2] << 8)
        raw += z[i + 5:i + 5 + n]
        i += 5 + n
        if final:
            break
    if rd32(z, i) != adler32(raw):
        raise DecodeError("png: adler32 mismatch")
    px = []
    for y in range(h):
        row = y * (w * 3 + 1)
        if raw[row] != 0:
            raise DecodeError(f"png: row {y} filter {raw[row]}")
        px += raw[row + 1:row + 1 + w * 3]
    return (w, h, px, kinds)


def paste(sheet, sw, thumb, ox, oy):
    w, h, px = thumb
    for y in range(h):
        for x in range(w):
            for c in range(3):
                di = ((oy + y) * sw + ox + x) * 3 + c
                sheet[di] = (sheet[di] + px[(y * w + x) * 3 + c] * 3) // 4


def main():
    tab = sine_table()
    names = ["mandel", "julia", "plasma", "terrain"]

    # Four renders, each from nothing but its own constants (the hot group).
    mandel = render_mandel()
    julia = render_julia()
    plasma = render_plasma(tab)
    terrain = render_terrain()

    # Four writes, one file per image.
    files = [f"thumb_{n}.ppm" for n in names]
    for path, px in zip(files, [mandel, julia, plasma, terrain]):
        with open(path, "wb") as f:
            f.write(encode_ppm(px, W, H))

    # A damaged copy, to show the decoder's error.
    with open(files[0], "rb") as f:
        whole = f.read()
    with open("thumb_broken.ppm", "wb") as f:
        f.write(whole[:1000])

    # Four reads and four decodes.
    datas = []
    for path in files:
        with open(path, "rb") as f:
            datas.append(f.read())
    tc = crc_table()
    for n, d in zip(names, datas):
        print(f"{n}: {len(d)} bytes, crc32 {crc32(tc, d, 0, len(d)):08x}")
    imgs = [decode_ppm(n, d) for n, d in zip(names, datas)]
    with open("thumb_broken.ppm", "rb") as f:
        broken = f.read()
    try:
        decode_ppm("broken", broken)
        print("broken: decoded")
    except DecodeError as e:
        print(e)

    # Four thumbnails.
    thumbs = [make_thumb(img) for img in imgs]

    # Reports that only read the images and thumbnails.
    for n, img in zip(names, imgs):
        print(histogram(n, img))
    for n, img in zip(names, imgs):
        print(channel_stats(n, img))
    for n, img in zip(names, imgs):
        print(edge_count(n, img))
    for n, th in zip(names, thumbs):
        print(palette(n, th))
    for n, th in zip(names, thumbs):
        print(preview(n, th))

    # The contact sheet: the thumbnails overlap, so the pastes run in order.
    sw = 76
    sh = 56
    sheet = [16] * (sw * sh * 3)
    paste(sheet, sw, thumbs[0], 0, 0)
    paste(sheet, sw, thumbs[1], 36, 0)
    paste(sheet, sw, thumbs[2], 0, 26)
    paste(sheet, sw, thumbs[3], 36, 26)
    png = encode_png(tc, sw, sh, sheet)
    with open("contact_sheet.png", "wb") as f:
        f.write(bytes(png))
    with open("contact_sheet.png", "rb") as f:
        back = list(f.read())
    print(f"contact_sheet.png: {len(back)} bytes, crc32 {crc32(tc, back, 0, len(back)):08x}")
    try:
        w, h, px, kinds = decode_png(tc, back)
        print(f"  chunks {' '.join(kinds)}, {w}x{h}, pixels {'match' if px == sheet else 'differ'}")
    except DecodeError as e:
        print(f"  {e}")
    bad = list(back)
    bad[40] ^= 0x10
    try:
        decode_png(tc, bad)
        print("  damaged copy: decoded")
    except DecodeError as e:
        print(f"  damaged copy: {e}")

    for path in files + ["thumb_broken.ppm", "contact_sheet.png"]:
        os.remove(path)


main()
