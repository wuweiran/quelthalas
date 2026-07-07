# Quel'Thalas

Quel'Thalas is a Rust libary for creating classic Win32 applications conforming to Fluent UI 2 design. It contains a set of UI components that can be easily integrated into any Win32 application developed in Rust.

## Usage

See [`sample/`](sample) for a complete, runnable application that creates buttons, inputs, a progress bar, a dialog, and a context menu.

Quel'Thalas is single-threaded: `QT` is neither `Send` nor `Sync`, its controls own windows and run a message loop, and it uses single-threaded Direct2D factories. Use it on your GUI thread, and initialize COM on that thread as a single-threaded apartment (STA) before creating a `QT` — as the sample does:

```rust
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?; }
let qt = quelthalas::QT::new()?;
```
