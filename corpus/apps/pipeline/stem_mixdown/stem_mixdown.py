# The Python mirror of stem_mixdown.kara, statement for statement; it
# wrote expected.out. Kara's `/` truncates toward zero, so every division
# that can see a negative operand goes through tdiv(); `>>` floors in both
# languages, and every `%` here has a non-negative left operand.
# Run as: python3 -I -B stem_mixdown.py > expected.out
import os

RATE = 16000
LEN = 32000
TAB = 1024
PEAK = 32767


class WavError(Exception):
    pass


def tdiv(a, b):
    q = abs(a) // abs(b)
    return q if (a < 0) == (b < 0) else -q


def sine_table():
    t = []
    h = TAB // 2
    for i in range(TAB):
        a = i % h
        v = PEAK * 16 * a * (h - a) // (5 * h * h - 4 * a * (h - a))
        t.append(v if i < h else -v)
    return t


def step_of(mhz):
    return mhz * TAB * 65536 // (RATE * 1000)


def at(tab, phase):
    return tab[(phase >> 16) % TAB]


def clamp16(v):
    if v > PEAK:
        return PEAK
    if v < -PEAK:
        return -PEAK
    return v


def lcg(state):
    return (state * 1103515245 + 12345) % 2147483648


def synth_voice(tab):
    out = []
    phase = 0
    vib = 0
    lp = 0
    vstep = step_of(5000)
    for n in range(LEN):
        f = 140000 + tdiv(4000 * at(tab, vib), PEAK)
        vib += vstep
        phase += step_of(f)
        saw = ((phase >> 16) % TAB) * 64 - 32768
        lp += tdiv((saw - lp) * 3, 16)
        t = n % 4000
        env = 0
        if t < 2880 and (n < 19200 or n >= 24000):
            edge = t if t < 2880 - t else 2880 - t
            env = PEAK if edge >= 400 else edge * PEAK // 400
        out.append(clamp16(lp * env >> 15))
    return out


def synth_music(tab):
    out = []
    s1, s2, s3, st = step_of(220000), step_of(277183), step_of(329628), step_of(3000)
    p1 = p2 = p3 = pt = 0
    for _ in range(LEN):
        chord = at(tab, p1) + at(tab, p2) + at(tab, p3)
        amp = 24000 + tdiv(6000 * at(tab, pt), PEAK)
        out.append(clamp16(tdiv(tdiv(chord, 3) * amp, PEAK)))
        p1 += s1
        p2 += s2
        p3 += s3
        pt += st
    return out


def synth_drums(tab):
    out = []
    phase = 0
    sweep = 0
    kamp = 0
    hamp = 0
    seed = 20241024
    prev = 0
    for n in range(LEN):
        if n % 8000 == 0:
            sweep = PEAK
            kamp = PEAK
            phase = 0
        if n % 4000 == 2000:
            hamp = 20000
        phase += step_of(50000 + (100000 * sweep >> 15))
        kick = at(tab, phase) * kamp >> 15
        sweep = sweep * 32700 >> 15
        kamp = kamp * 32740 >> 15
        seed = lcg(seed)
        noise = ((seed >> 15) % 65536) - 32768
        hat = tdiv(noise - prev, 2) * hamp >> 15
        prev = noise
        hamp = hamp * 32000 >> 15
        out.append(clamp16(kick + tdiv(hat, 3)))
    return out


def synth_room(tab):
    out = []
    seed = 7
    lp = 0
    hum = 0
    hstep = step_of(60000)
    for _ in range(LEN):
        seed = lcg(seed)
        noise = ((seed >> 15) % 65536) - 32768
        lp += tdiv(noise - lp, 32)
        out.append(clamp16(tdiv(lp, 4) + tdiv(at(tab, hum) * 800, PEAK)))
        hum += hstep
    return out


def put_u16(out, v):
    out.append(v % 256)
    out.append(v // 256 % 256)


def put_u32(out, v):
    put_u16(out, v % 65536)
    put_u16(out, v // 65536)


def encode_wav(s):
    out = bytearray()
    data = len(s) * 2
    out += b"RIFF"
    put_u32(out, 36 + data)
    out += b"WAVE"
    out += b"fmt "
    put_u32(out, 16)
    put_u16(out, 1)
    put_u16(out, 1)
    put_u32(out, RATE)
    put_u32(out, RATE * 2)
    put_u16(out, 2)
    put_u16(out, 16)
    out += b"data"
    put_u32(out, data)
    for v in s:
        put_u16(out, v + 65536 if v < 0 else v)
    return bytes(out)


def get_u16(b, i):
    return b[i] + b[i + 1] * 256


def get_u32(b, i):
    return get_u16(b, i) + get_u16(b, i + 2) * 65536


def tag_at(b, i):
    try:
        return b[i:i + 4].decode("utf-8")
    except UnicodeDecodeError:
        return "????"


def decode_wav(name, b):
    n = len(b)
    if n < 12:
        raise WavError(f"{name}: short header ({n} bytes)")
    if tag_at(b, 0) != "RIFF" or tag_at(b, 8) != "WAVE":
        raise WavError(f"{name}: not a RIFF WAVE file")
    off = 12
    have_fmt = False
    while off + 8 <= n:
        cid = tag_at(b, off)
        size = get_u32(b, off + 4)
        if cid == "fmt ":
            fmt = get_u16(b, off + 8)
            channels = get_u16(b, off + 10)
            rate = get_u32(b, off + 12)
            bits = get_u16(b, off + 22)
            if fmt != 1:
                raise WavError(f"{name}: unsupported format {fmt}")
            if channels != 1 or bits != 16 or rate != RATE:
                raise WavError(f"{name}: expected mono 16-bit at {RATE} Hz, got {channels} channel(s), {bits}-bit at {rate} Hz")
            have_fmt = True
        elif cid == "data":
            if not have_fmt:
                raise WavError(f"{name}: data chunk before fmt chunk")
            avail = n - off - 8
            if size > avail:
                raise WavError(f"{name}: data chunk claims {size} bytes, file has {avail}")
            s = []
            i = off + 8
            while i + 1 < off + 8 + size:
                v = get_u16(b, i)
                s.append(v - 65536 if v >= 32768 else v)
                i += 2
            return s
        off += 8 + size + size % 2
    raise WavError(f"{name}: no data chunk")


def crc_table():
    t = []
    for n in range(256):
        c = n
        for _ in range(8):
            c = 3988292384 ^ (c // 2) if c % 2 == 1 else c // 2
        t.append(c)
    return t


def crc32(t, d):
    c = 4294967295
    for b in d:
        c = t[(c ^ b) % 256] ^ (c // 256)
    return c ^ 4294967295


def file_line(name, t, d):
    return f"{name}: {len(d)} bytes, crc32 {crc32(t, d):08x}"


def clean(name, x):
    y = []
    xp = yp = env = 0
    for v in x:
        h = v - xp + (32604 * yp >> 15)
        xp = v
        yp = h
        a = abs(h)
        decayed = env * 32680 >> 15
        env = a if a > decayed else decayed
        g = (12000 + (env - 12000) // 4) * 32768 // env if env > 12000 else 32768
        y.append(h * g >> 15)
    peak = 0
    for v in y:
        if abs(v) > peak:
            peak = abs(v)
    out = [tdiv(v * 24000, peak) if peak > 0 else 0 for v in y]
    return (name, out)


def isqrt(n):
    if n < 2:
        return n
    x = n
    y = (x + 1) // 2
    while y < x:
        x = y
        y = (x + n // x) // 2
    return x


def levels(st):
    name, s = st
    peak = 0
    sq = 0
    for v in s:
        if abs(v) > peak:
            peak = abs(v)
        sq += v * v
    rms = isqrt(sq // len(s))
    crest = peak * 10 // rms if rms > 0 else 0
    return f"{name} levels: peak {peak}, rms {rms}, crest {crest // 10}.{crest % 10}"


def crossings(st):
    name, s = st
    count = 0
    for i in range(1, len(s)):
        if (s[i - 1] < 0) != (s[i] < 0):
            count += 1
    return f"{name} zero crossings: {count}, {count * RATE // len(s)} per second"


def spectrum(st, tab):
    name, s = st
    n = 4096
    parts = []
    for f in [60, 140, 220, 330, 500, 1000, 2000, 4000]:
        k = f * TAB // RATE
        coeff = tab[(k + TAB // 4) % TAB]
        s1 = s2 = 0
        for i in range(n):
            v = s[i] + (coeff * s1 >> 14) - s2
            s2 = s1
            s1 = v
        power = s1 * s1 + s2 * s2 - (coeff * s1 >> 14) * s2
        mag = isqrt(power) * 2 // n if power > 0 else 0
        parts.append(f"{f}:{mag}")
    return f"{name} spectrum: {' '.join(parts)}"


def silences(st):
    name, s = st
    w = 320
    windows = len(s) // w
    runs = []
    start = -1
    for k in range(windows + 1):
        quiet = False
        if k < windows:
            m = 0
            for i in range(k * w, (k + 1) * w):
                if abs(s[i]) > m:
                    m = abs(s[i])
            quiet = m < 800
        if quiet and start < 0:
            start = k
        elif not quiet and start >= 0:
            if k - start >= 5:
                runs.append(f"{start * 20}-{k * 20} ms")
            start = -1
    if not runs:
        return f"{name} silences: none"
    return f"{name} silences: {', '.join(runs)}"


def waveform(st):
    name, s = st
    ramp = " .:-=+*#%@"
    cols = 50
    per = len(s) // cols
    line = []
    for c in range(cols):
        m = 0
        for i in range(c * per, (c + 1) * per):
            if abs(s[i]) > m:
                m = abs(s[i])
        line.append(ramp[m * 10 // 32768])
    return f"{name} waveform: |{''.join(line)}|"


def mix_into(bus, st, gain):
    _, s = st
    clipped = 0
    for i in range(len(bus)):
        v = bus[i] + (s[i] * gain >> 15)
        if v > PEAK or v < -PEAK:
            clipped += 1
        bus[i] = clamp16(v)
    return clipped


def loudness(bus):
    w = 4000
    parts = []
    for k in range(len(bus) // w):
        sq = 0
        for i in range(k * w, (k + 1) * w):
            sq += bus[i] * bus[i]
        parts.append(f"{isqrt(sq // w)}")
    return f"master rms per 250 ms: {' '.join(parts)}"


def read_file(path):
    with open(path, "rb") as f:
        return f.read()


def write_file(path, data):
    with open(path, "wb") as f:
        f.write(data)


def run():
    tab = sine_table()
    tc = crc_table()
    voice = synth_voice(tab)
    music = synth_music(tab)
    drums = synth_drums(tab)
    room = synth_room(tab)
    write_file("stem_voice.wav", encode_wav(voice))
    write_file("stem_music.wav", encode_wav(music))
    write_file("stem_drums.wav", encode_wav(drums))
    write_file("stem_room.wav", encode_wav(room))
    whole = read_file("stem_voice.wav")
    write_file("stem_cut.wav", whole[0:1000])
    d1 = read_file("stem_voice.wav")
    d2 = read_file("stem_music.wav")
    d3 = read_file("stem_drums.wav")
    d4 = read_file("stem_room.wav")
    print(file_line("stem_voice.wav", tc, d1))
    print(file_line("stem_music.wav", tc, d2))
    print(file_line("stem_drums.wav", tc, d3))
    print(file_line("stem_room.wav", tc, d4))
    v = decode_wav("voice", d1)
    m = decode_wav("music", d2)
    d = decode_wav("drums", d3)
    r = decode_wav("room", d4)
    try:
        s = decode_wav("cut", read_file("stem_cut.wav"))
        print(f"cut: decoded {len(s)} samples")
    except WavError as e:
        print(e)
    cv = clean("voice", v)
    cm = clean("music", m)
    cd = clean("drums", d)
    cr = clean("room", r)
    lines = [levels(x) for x in (cv, cm, cd, cr)]
    lines += [crossings(x) for x in (cv, cm, cd, cr)]
    lines += [spectrum(x, tab) for x in (cv, cm, cd, cr)]
    lines += [silences(x) for x in (cv, cm, cd, cr)]
    lines += [waveform(x) for x in (cv, cm, cd, cr)]
    for line in lines:
        print(line)
    bus = [0] * LEN
    k1 = mix_into(bus, cv, 32767)
    k2 = mix_into(bus, cm, 19660)
    k3 = mix_into(bus, cd, 29490)
    k4 = mix_into(bus, cr, 9830)
    print(f"mix: voice clipped {k1}, music {k2}, drums {k3}, room {k4}")
    print(loudness(bus))
    master = ("master", bus)
    print(levels(master))
    print(waveform(master))
    write_file("master.wav", encode_wav(bus))
    back = read_file("master.wav")
    print(file_line("master.wav", tc, back))
    try:
        s = decode_wav("master", back)
        print(f"  {len(s)} samples, {'match' if s == bus else 'differ'}")
    except WavError as e:
        print(f"  {e}")
    bad = bytearray(back)
    bad[20] = 3
    try:
        decode_wav("damaged master", bytes(bad))
        print("  damaged copy: decoded")
    except WavError as e:
        print(f"  {e}")
    for path in ["stem_voice.wav", "stem_music.wav", "stem_drums.wav", "stem_room.wav", "stem_cut.wav", "master.wav"]:
        os.remove(path)


run()
