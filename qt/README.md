# Quel'Thalas

Quel'Thalas is a Rust library for building classic Win32 applications that
conform to the [Fluent UI 2](https://fluent2.microsoft.design/) design language.
It reimplements common controls from scratch on real child `HWND`s, painting them
with Direct2D and DirectWrite — keeping native Win32 behavior while restyling the
look.

## Components

`button`, `checkbox`, `combobox`, `dialog`, `divider`, `dropdown`, `input`,
`link`, `list_box`, `menu`, `menu_bar`, `option`, `progress_bar`, `radio`,
`slider`, `spin_button`, `spinner`, `split_button`, `switch`, `tab_list`,
`task_dialog`, `text`, `textarea`, `tooltip`, `tree_view`, plus a shared `scroll`
helper (a WinUI 3-style scrollbar) that self-painting controls embed.

## Platform

Windows only. The `windows` crate that Quel'Thalas depends on does not build on
other targets.

## Usage

Quel'Thalas is single-threaded: `QT` is neither `Send` nor `Sync`, its controls
own windows and run a message loop, and it uses single-threaded Direct2D
factories. Use it on your GUI thread, and initialize COM on that thread as a
single-threaded apartment (STA) before creating a `QT`:

```rust
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?; }
let qt = quelthalas::QT::new()?;
```

See [`sample/`](https://github.com/wuweiran/quelthalas/tree/main/sample) for a
complete, runnable application that exercises the components — buttons, inputs, a
textarea with a scrollbar, a combobox, a dialog, a context menu, and more.

## Theming

`QT::new()` uses the light theme. Construct with an explicit theme via
`QT::new_with(Theme)` — `Theme::web_light()` / `Theme::web_dark()` — and read
Fluent design tokens through `qt.theme().tokens`.

## License

Licensed under the [MIT License](LICENSE).
