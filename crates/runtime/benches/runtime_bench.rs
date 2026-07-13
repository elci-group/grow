use criterion::{Criterion, criterion_group, criterion_main};

const ECHO_WAT: &str = r#"(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "200 OK\0aContent-Type: text/plain\0a\0agrow wasm says: ")
  (func (export "_start")
    (i32.store (i32.const 1024) (i32.const 256))
    (i32.store (i32.const 1028) (i32.const 512))
    (drop (call $fd_read (i32.const 0) (i32.const 1024) (i32.const 1) (i32.const 1032)))
    (i32.store (i32.const 1040) (i32.const 0))
    (i32.store (i32.const 1044) (i32.const 49))
    (drop (call $fd_write (i32.const 1) (i32.const 1040) (i32.const 1) (i32.const 1048)))
    (i32.store (i32.const 1056) (i32.const 256))
    (i32.store (i32.const 1060) (i32.load (i32.const 1032)))
    (drop (call $fd_write (i32.const 1) (i32.const 1056) (i32.const 1) (i32.const 1064)))
    (call $proc_exit (i32.const 0))
  )
)"#;

fn bench_wasm_invocation(c: &mut Criterion) {
    let wasm = wat::parse_str(ECHO_WAT).expect("WAT compiles");
    let runtime = grow_runtime::Runtime::new(&wasm).expect("runtime builds");
    let request = grow_runtime::Request {
        method: "GET".into(),
        path: "/api/hello".into(),
        headers: vec![("Host".into(), "localhost".into())],
        body: Vec::new(),
    };

    c.bench_function("wasm_invoke_get", |b| {
        b.iter(|| runtime.invoke(&request).expect("invoke ok"))
    });
}

fn bench_wasm_compile(c: &mut Criterion) {
    let wasm = wat::parse_str(ECHO_WAT).expect("WAT compiles");
    c.bench_function("wasm_compile", |b| {
        b.iter(|| grow_runtime::Runtime::new(&wasm).expect("compile ok"))
    });
}

fn bench_response_parse(c: &mut Criterion) {
    let response = b"200 OK\nContent-Type: text/plain\n\nbody content here";
    c.bench_function("parse_response", |b| {
        b.iter(|| grow_runtime::parse_response(response).expect("parse ok"))
    });
}

criterion_group!(
    benches,
    bench_wasm_invocation,
    bench_wasm_compile,
    bench_response_parse
);
criterion_main!(benches);
