use std::hint::black_box;
fn build(pattern: &str, n: i64) -> Vec<(i64, i64)> {
    let mut v: Vec<(i64,i64)> = Vec::new();
    let mut seed: i64 = 12345; let mut i: i64 = 0;
    while i < n {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        let r = seed;
        let k: i64 = match pattern {
            "few_unique" => r % 8, "sawtooth" => i % 1000, "random" => r, _ => unreachable!() };
        v.push((k, i)); i += 1;
    }
    v
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let p = a[1].as_str();
    let do_sort = a[2] == "sort";
    let mut w = black_box(build(p, 150_000));
    if do_sort { w.sort_by(|x, y| x.0.cmp(&y.0)); }
    black_box(&w);
    println!("{}", w[0].0);
}
