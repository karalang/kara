// wasm_size bench — the Iris filter core in Rust.
//
// A faithful port of ../../kara/filter_core.kara (itself a verbatim port of
// examples/iris/src/filters.kara). Integer math throughout, so the FNV-1a
// checksums printed here MUST byte-match the Kāra and TinyGo ports — that is the
// cross-language correctness check that makes the module-size comparison honest.

fn img_width() -> i64 {
    512
}
fn img_height() -> i64 {
    384
}

fn f_blur() -> i64 {
    1
}
fn f_sharpen() -> i64 {
    2
}
fn f_edge() -> i64 {
    3
}
fn f_invert() -> i64 {
    4
}
fn f_grayscale() -> i64 {
    5
}
fn filter_count() -> i64 {
    6
}

fn clampi(v: i64, lo: i64, hi: i64) -> i64 {
    if v < lo {
        return lo;
    }
    if v > hi {
        return hi;
    }
    v
}

fn to_byte(v: i64) -> u8 {
    clampi(v, 0, 255) as u8
}

fn source_channel(x0: i64, y0: i64, ch: i64) -> i64 {
    let w = img_width();
    let h = img_height();
    let x = clampi(x0, 0, w - 1);
    let y = clampi(y0, 0, h - 1);
    let cx = w / 2;
    let cy = h / 2;
    let radius = h / 4;

    let mut r = (x * 255) / w;
    let mut g = (y * 255) / h;
    let mut b = ((x + y) * 255) / (w + h);

    let dx = x - cx;
    let dy = y - cy;
    if dx * dx + dy * dy < radius * radius {
        r = 250;
        g = 230;
        b = 40;
    }

    if x < cx && y < cy && ((x / 24) + (y / 24)) % 2 == 0 {
        r = 20;
        g = 20;
        b = 30;
    }

    if x >= cx && y >= cy && ((x + y) / 16) % 2 == 0 {
        r = 230;
        g = 60;
        b = 90;
    }

    if ch == 0 {
        return clampi(r, 0, 255);
    }
    if ch == 1 {
        return clampi(g, 0, 255);
    }
    clampi(b, 0, 255)
}

fn luma(x: i64, y: i64) -> i64 {
    let r = source_channel(x, y, 0);
    let g = source_channel(x, y, 1);
    let b = source_channel(x, y, 2);
    (r * 77 + g * 150 + b * 29) / 256
}

fn blur_channel(x: i64, y: i64, ch: i64) -> i64 {
    let mut acc = 0;
    let mut dy = -1;
    while dy <= 1 {
        let mut dx = -1;
        while dx <= 1 {
            acc += source_channel(x + dx, y + dy, ch);
            dx += 1;
        }
        dy += 1;
    }
    acc / 9
}

fn sharpen_channel(x: i64, y: i64, ch: i64) -> i64 {
    let c = source_channel(x, y, ch) * 5;
    let n = source_channel(x, y - 1, ch);
    let s = source_channel(x, y + 1, ch);
    let e = source_channel(x + 1, y, ch);
    let west = source_channel(x - 1, y, ch);
    c - n - s - e - west
}

fn sobel(x: i64, y: i64) -> i64 {
    let tl = luma(x - 1, y - 1);
    let tc = luma(x, y - 1);
    let tr = luma(x + 1, y - 1);
    let ml = luma(x - 1, y);
    let mr = luma(x + 1, y);
    let bl = luma(x - 1, y + 1);
    let bc = luma(x, y + 1);
    let br = luma(x + 1, y + 1);
    let gx = (tr + 2 * mr + br) - (tl + 2 * ml + bl);
    let gy = (bl + 2 * bc + br) - (tl + 2 * tc + tr);
    let mag2 = (gx * gx + gy * gy) as f64;
    mag2.sqrt() as i64
}

fn render_band(y0: i64, y1: i64, filter_id: i64) -> Vec<u8> {
    let w = img_width();
    let mut out: Vec<u8> = Vec::new();
    for y in y0..y1 {
        for x in 0..w {
            if filter_id == f_blur() {
                out.push(to_byte(blur_channel(x, y, 0)));
                out.push(to_byte(blur_channel(x, y, 1)));
                out.push(to_byte(blur_channel(x, y, 2)));
            } else if filter_id == f_sharpen() {
                out.push(to_byte(sharpen_channel(x, y, 0)));
                out.push(to_byte(sharpen_channel(x, y, 1)));
                out.push(to_byte(sharpen_channel(x, y, 2)));
            } else if filter_id == f_edge() {
                let m = to_byte(sobel(x, y));
                out.push(m);
                out.push(m);
                out.push(m);
            } else if filter_id == f_invert() {
                out.push(to_byte(255 - source_channel(x, y, 0)));
                out.push(to_byte(255 - source_channel(x, y, 1)));
                out.push(to_byte(255 - source_channel(x, y, 2)));
            } else if filter_id == f_grayscale() {
                let l = to_byte(luma(x, y));
                out.push(l);
                out.push(l);
                out.push(l);
            } else {
                out.push(to_byte(source_channel(x, y, 0)));
                out.push(to_byte(source_channel(x, y, 1)));
                out.push(to_byte(source_channel(x, y, 2)));
            }
            out.push(255u8);
        }
    }
    out
}

fn apply_full(filter_id: i64) -> Vec<u8> {
    render_band(0, img_height(), filter_id)
}

// FNV-1a folded to 32 bits — u32 wrapping mul is exactly the Kāra port's
// `wrapping_mul(16777619) % 4294967296`.
fn checksum(buf: &[u8]) -> u32 {
    let mut h: u32 = 2166136261;
    for &b in buf {
        h = (h ^ (b as u32)).wrapping_mul(16777619);
    }
    h
}

fn main() {
    let mut id = 0;
    while id < filter_count() {
        let out = apply_full(id);
        let c = checksum(&out);
        println!("filter {} checksum {}", id, c);
        id += 1;
    }
}
