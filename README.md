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
- **Rust 1.85+** (edition 2024).

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
