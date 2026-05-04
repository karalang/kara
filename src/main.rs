fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = karac::cli::parse_args(&args);
    karac::cli::execute(cmd);
}
