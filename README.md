# Quel'Thalas

A Rust workspace for building classic Win32 desktop applications that conform to
the [Fluent UI 2](https://fluent2.microsoft.design/) design language.

Quel'Thalas reimplements common Win32 controls from scratch on real child
`HWND`s, painting them with Direct2D and DirectWrite. The guiding thesis is
**keep the native Win32 behavior, restyle the look** — every control is rebuilt
rather than wrapping `comctl32`, so it behaves like the real thing while wearing
Fluent's visuals. Public naming follows [Fluent
React](https://react.fluentui.dev/) conventions.

## Repository layout

```
quelthalas/
├── qt/                 # the `quelthalas` library crate (published to crates.io)
│   ├── src/
│   │   ├── lib.rs      # QT entry point, theme accessor
│   │   ├── theme.rs    # Fluent design tokens (light/dark) + typography
│   │   ├── layout.rs   # Stack layout helper
│   │   ├── component/  # one module per control (button, input, textarea, …)
│   │   └── icon/       # embedded Fluent SVG icons
│   ├── README.md       # the crate-level README (library usage)
│   └── LICENSE
└── sample/             # a runnable demo binary exercising every component
    └── src/main.rs
```

The library exposes 25 components. See
[`qt/README.md`](qt/README.md#components) for the full list.

## Requirements

- **Windows only.** The `windows` crate this project depends on does not build on
  other targets.
- **Rust 1.85+** (edition 2024) for a standard build.
- **Runtime:** the default build (stable Rust, `*-pc-windows-msvc`) targets
  **Windows 10 or later** — not because of this library (its code runs on Windows
  7), but because the prebuilt Rust standard library for those targets links
  Windows 10-only APIs. See [Windows 7](#windows-7) below to build for Windows 7.

## Windows 7

Quel'Thalas runs on **Windows 7 SP1**. The library deliberately stays on
Windows-7-era APIs — icons are Direct2D 1.0 path geometry (not Direct2D SVG
documents), per-monitor DPI is resolved at runtime via `GetProcAddress` with a
system-DPI fallback, and animations use the v1 Windows Animation Manager with a
custom cubic-bezier interpolator.

The only Windows 10 floor is the **prebuilt Rust `std`** for the default MSVC
targets. Building for Windows 7 therefore uses Rust's tier-3
`*-win7-windows-msvc` target (which compiles `std` from source), a static CRT,
and one post-link patch:

```sh
# One-time setup (nightly + std source for build-std)
rustup toolchain install nightly --component rust-src

# Build a self-contained 32-bit Windows 7 binary (runs on 32- and 64-bit Win7)
RUSTFLAGS="-C target-feature=+crt-static" \
  cargo +nightly build --release -Z build-std=std,panic_abort \
  --target i686-win7-windows-msvc -p sample

# Redirect the one Win8+ import (combase.dll) to its Windows 7 home (ole32.dll)
python3 scripts/patch-combase-to-ole32.py \
  target/i686-win7-windows-msvc/release/sample.exe

# Optional: verify the binary has no Windows 7 load blockers (needs MSVC dumpbin)
scripts/check-win7-imports.sh target/i686-win7-windows-msvc/release/sample.exe
```

On the Windows 7 machine, install the **Platform Update for Windows 7 SP1
([KB2670838](https://www.catalog.update.microsoft.com/Search.aspx?q=KB2670838))**
first — it provides Direct2D 1.1, which the render targets require. (Verify
`C:\Windows\System32\d2d1.dll` reports version **6.2.x**.) A GPU-less VM falls
back to WARP software rendering, which is fine.

## Building and running

Run the sample application (the fastest way to see everything):

```sh
cargo run -p sample
```

Build just the library:

```sh
cargo build -p quelthalas
```

## Using the library

`quelthalas` is published to crates.io. See [`qt/README.md`](qt/README.md) for
library usage, the single-threaded/STA requirements, and theming.

## License

Licensed under the [MIT License](qt/LICENSE).
