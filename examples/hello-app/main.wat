;; hello-app entry module.
;;
;; CGI-style over WASI preview1: reads the request frame from stdin
;; (`METHOD PATH\n`, headers, blank line, body) and writes the response
;; (`STATUS REASON\n`, headers, blank line, body) to stdout.
;;
;; Memory layout: response head at 0 (69 bytes), request buffer at
;; [256, 768), iovecs and scratch at 1024+.
(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "200 OK\0aContent-Type: text/plain\0a\0ahello from GROW wasm\0ayou requested: ")
  (func (export "_start")
    ;; read request (fd 0) into [256, 768), iovec at 1024, nread at 1032
    (i32.store (i32.const 1024) (i32.const 256))
    (i32.store (i32.const 1028) (i32.const 512))
    (drop (call $fd_read (i32.const 0) (i32.const 1024) (i32.const 1) (i32.const 1032)))
    ;; write response head (fd 1), iovec at 1040, nwritten at 1048
    (i32.store (i32.const 1040) (i32.const 0))
    (i32.store (i32.const 1044) (i32.const 69))
    (drop (call $fd_write (i32.const 1) (i32.const 1040) (i32.const 1) (i32.const 1048)))
    ;; echo the request frame, iovec at 1056, nwritten at 1064
    (i32.store (i32.const 1056) (i32.const 256))
    (i32.store (i32.const 1060) (i32.load (i32.const 1032)))
    (drop (call $fd_write (i32.const 1) (i32.const 1056) (i32.const 1) (i32.const 1064)))
    (call $proc_exit (i32.const 0))
  )
)
