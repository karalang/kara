fn main() {
    let path = std::env::args().nth(1).unwrap();
    let src = std::fs::read_to_string(&path).unwrap();
    let mut parsed = karac::parse(&src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    match karac::codegen::compile_to_ir(&parsed.program, None, None) {
        Ok(ir) => println!("{ir}"),
        Err(e) => eprintln!("ERR {e}"),
    }
}
