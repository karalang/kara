//! Per-cell pipeline cost measurement for the REPL JIT path.
//!
//! Mirrors `Session::run_with_wrapper_inner`'s parse → resolve →
//! typecheck → ownership → lower → codegen-IR phases at growing
//! program sizes. The simulated REPL is a 20-cell session where each
//! cell defines a small fn and then calls all prior fns from `main`
//! — what a debugging / data-exploration session might look like
//! after twenty incremental refinements.
//!
//! Run with:
//!   cargo run --release --example bench_repl_amortization --features llvm
//!
//! Reports per-cell wall time, then a phase breakdown averaged over
//! the last few cells (where amortization savings would be largest).

use std::time::Instant;

const CELLS: usize = 50;

fn item_source(idx: usize) -> String {
    // A larger item per cell: fn with Vec[i64], Map[String, i64], control
    // flow, generic call. Closer in shape to a realistic data-exploration
    // REPL cell ("define a helper, use it from main").
    format!(
        "fn cell{idx}_helper(seed: i64) -> i64 {{\n\
         \tlet mut v: Vec[i64] = Vec.new();\n\
         \tlet mut m: Map[String, i64] = Map.new();\n\
         \tlet mut i = 0;\n\
         \twhile i < 8 {{\n\
         \t\tv.push(seed + i);\n\
         \t\tlet k = f\"k{{i}}\";\n\
         \t\tm.insert(k, seed * (i + 1));\n\
         \t\ti = i + 1;\n\
         \t}}\n\
         \tlet mut acc: i64 = 0;\n\
         \tfor x in v {{\n\
         \t\tacc = acc + x;\n\
         \t}}\n\
         \tacc\n\
         }}\n"
    )
}

fn main_body(prior_count: usize) -> String {
    let mut body = String::from("fn main() {\n");
    body.push_str("\tlet mut total: i64 = 0;\n");
    for i in 0..prior_count {
        body.push_str(&format!("\ttotal = total + cell{i}_helper({});\n", i + 1));
    }
    body.push_str("\tprintln(total);\n");
    body.push_str("}\n");
    body
}

fn run_phases(synthetic: &str) -> Phases {
    let t0 = Instant::now();
    let mut parsed = karac::parse(synthetic);
    let t_parse = t0.elapsed().as_micros();
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );

    let t1 = Instant::now();
    let resolved = karac::resolve(&parsed.program);
    let t_resolve = t1.elapsed().as_micros();
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );

    let t2 = Instant::now();
    let typed = karac::typecheck(&parsed.program, &resolved);
    let t_typecheck = t2.elapsed().as_micros();
    assert!(
        typed.errors.is_empty(),
        "typecheck errors: {:?}",
        typed.errors
    );

    let t3 = Instant::now();
    let owned = karac::ownershipcheck(&parsed.program, &typed);
    let t_owned = t3.elapsed().as_micros();
    assert!(
        owned.errors.is_empty(),
        "ownership errors: {:?}",
        owned.errors
    );

    let t4 = Instant::now();
    karac::lower(&mut parsed.program, &typed);
    let t_lower = t4.elapsed().as_micros();

    let t5 = Instant::now();
    #[cfg(feature = "llvm")]
    let _ir = karac::codegen::compile_to_ir_with_options(&parsed.program, None, None, None, None)
        .expect("compile_to_ir");
    let t_codegen = t5.elapsed().as_micros();

    Phases {
        parse: t_parse,
        resolve: t_resolve,
        typecheck: t_typecheck,
        ownership: t_owned,
        lower: t_lower,
        codegen: t_codegen,
    }
}

#[derive(Default, Clone, Copy)]
struct Phases {
    parse: u128,
    resolve: u128,
    typecheck: u128,
    ownership: u128,
    lower: u128,
    codegen: u128,
}

impl Phases {
    fn total(&self) -> u128 {
        self.parse + self.resolve + self.typecheck + self.ownership + self.lower + self.codegen
    }
}

fn main() {
    println!("== REPL per-cell pipeline cost ({CELLS} cells, growing items_source) ==\n");
    println!(
        "{:>4} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "cell", "bytes", "parse_us", "resolve", "typeck", "owned", "lower", "codegen", "TOTAL"
    );

    let mut items_source = String::new();
    let mut all: Vec<Phases> = Vec::with_capacity(CELLS);

    for cell in 0..CELLS {
        items_source.push_str(&item_source(cell));
        let synthetic = format!("{items_source}{}", main_body(cell + 1));
        let p = run_phases(&synthetic);
        all.push(p);
        println!(
            "{:>4} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
            cell,
            synthetic.len(),
            p.parse,
            p.resolve,
            p.typecheck,
            p.ownership,
            p.lower,
            p.codegen,
            p.total()
        );
    }

    println!("\n== Average across last 5 cells (post-warmup, full accumulated state) ==");
    let n = 5usize.min(all.len());
    let last = &all[all.len() - n..];
    let mean =
        |sel: fn(&Phases) -> u128| -> u128 { last.iter().map(sel).sum::<u128>() / n as u128 };
    let parse = mean(|p| p.parse);
    let resolve = mean(|p| p.resolve);
    let typecheck = mean(|p| p.typecheck);
    let ownership = mean(|p| p.ownership);
    let lower = mean(|p| p.lower);
    let codegen = mean(|p| p.codegen);
    let total = parse + resolve + typecheck + ownership + lower + codegen;
    println!("  parse  resolve typecheck ownership   lower  codegen   TOTAL  (microseconds)");
    println!(
        "{:>7} {:>8} {:>9} {:>9} {:>7} {:>8} {:>7}",
        parse, resolve, typecheck, ownership, lower, codegen, total
    );

    println!(
        "\nUpper-bound amortization savings if resolve+typecheck+ownership cached:\n  {} / {} us = {:.1}% of last-cell cost",
        resolve + typecheck + ownership,
        total,
        100.0 * (resolve + typecheck + ownership) as f64 / total as f64
    );
    println!(
        "Codegen-IR alone:\n  {} / {} us = {:.1}% of last-cell cost",
        codegen,
        total,
        100.0 * codegen as f64 / total as f64
    );
}
