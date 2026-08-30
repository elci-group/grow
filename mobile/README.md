# Grow Browser Mobile

An Android browser where `grow://` is a first-class scheme — no extension,
no keyword, no desktop daemon. The address bar takes
`grow://github.com/<owner>/<repo>` directly; the embedded `grow-mobile`
Rust engine clones the repository over https (pure-Rust gix, no git binary),
verifies the HEAD commit and manifest exactly like the desktop daemon, and
serves the site on a device-local port that the WebView renders.

Milestone 1 serves static sites (all current demos). Tabs, history, and
bookmarks are included. WASM execution on device is milestone 2.

## Layout

- `app/` — the Android application (Kotlin, plain framework views over a
  WebView; no AndroidX). `MainActivity` owns tabs, the address bar,
  bookmarks, and history; `Engine` is the JNI bridge.
- `app/src/main/jniLibs/` — the compiled `libgrow_mobile.so` per ABI,
  produced by `cargo ndk` (not committed).
- `../crates/mobile/` — the Rust engine (`grow-mobile`): `grow://` parsing
  via the shared `grow_server::resolve` grammar, gix https clone, manifest
  verification, static-only serving on `127.0.0.1`.

## Build

Prerequisites: Android SDK (platform 36, build-tools), NDK r26+, Rust
Android targets (`rustup target add aarch64-linux-android
armv7-linux-androideabi x86_64-linux-android`), and
[`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk).

```sh
# 1. Native engine for all ABIs (release)
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 \
  -o mobile/app/src/main/jniLibs build --release -p grow-mobile

# 2. The APK
cd mobile && ./gradlew :app:assembleDebug

# 3. Install on a connected device / emulator
adb install app/build/outputs/apk/debug/app-debug.apk
```

`gradlew` is the Gradle 8.14.3 wrapper; AGP 8.13 + Kotlin 2.2.20,
minSdk 26, targetSdk 36.

## Trust model

Identical to the desktop daemon: repositories are cloned once and never
auto-updated, the served tree is pinned to the verified HEAD commit, and
only `github.com` / `gitlab.com` https remotes are accepted (no
credentials in URLs). The WebView's cleartext allowance exists solely for
the engine's `http://127.0.0.1:<port>` app servers; remote content is
https-only.
