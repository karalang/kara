//! Print the reduction shader `gpu_wgsl::emit_reduce_kernel` generates, so the
//! runtime's hand-validated copy can be kept honest against it.
//!
//! Run: `cargo run --features llvm --example dump_reduce_shader -- min f32`

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let op = args.get(1).map(String::as_str).unwrap_or("sum");
    let elem = args.get(2).map(String::as_str).unwrap_or("f32");
    if op == "dot" {
        println!("{}", karac::gpu_wgsl::emit_dot_kernel(elem).unwrap());
        return;
    }
    let op = match op {
        "sum" => karac::reduce_kernel::ReduceOp::Sum,
        "prod" => karac::reduce_kernel::ReduceOp::Prod,
        "min" => karac::reduce_kernel::ReduceOp::Min,
        "max" => karac::reduce_kernel::ReduceOp::Max,
        other => panic!("unknown op {other}"),
    };
    println!("{}", karac::gpu_wgsl::emit_reduce_kernel(op, elem).unwrap());
}
