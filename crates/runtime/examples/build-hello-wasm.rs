//! Compiles `examples/hello-app/main.wat` into `main.wasm`.
//!
//! Run: `cargo run -p grow-runtime --example build-hello-wasm`
//!
//! The repository root `.gitignore` excludes the generated `main.wasm`; a
//! real application would commit a module built by its own toolchain.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/hello-app");
    let wat_text = std::fs::read_to_string(format!("{dir}/main.wat"))?;
    let wasm = wat::parse_str(&wat_text)?;
    std::fs::write(format!("{dir}/main.wasm"), &wasm)?;
    println!("wrote {dir}/main.wasm ({} bytes)", wasm.len());
    Ok(())
}
