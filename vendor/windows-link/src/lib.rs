//! Linking for Windows — **vendored copy** of `windows-link` 0.2.1.
//!
//! Identical to upstream, except: on the tier-3 Windows 7 targets
//! (`*-win7-windows-msvc`, which set `target_vendor = "win7"`) the `link!` macro
//! redirects imports named `combase.dll` to `ole32.dll`.
//!
//! Why: `windows-core`'s COM-object runtime (used by `#[implement]`) imports
//! `CoCreateFreeThreadedMarshaler` from `combase.dll` via `raw-dylib`, which bakes
//! that literal DLL name into the import table with no fallback. `combase.dll` is
//! Windows 8+; the same classic-OLE exports live in `ole32.dll` on every Windows
//! version, including 7. Rewriting the name here — at link time — makes the binary
//! load on Windows 7 with no post-build patching. Non-Win7 builds are byte-identical
//! to upstream (the redirect arm is `cfg`-gated away).
#![no_std]

// --- Windows 7 targets: redirect combase.dll -> ole32.dll, else pass through. ---

/// Defines an external function to import.
#[cfg(all(windows, target_vendor = "win7", target_arch = "x86"))]
#[macro_export]
macro_rules! link {
    ("combase.dll" $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        #[link(name = "ole32.dll", kind = "raw-dylib", modifiers = "+verbatim", import_name_type = "undecorated")]
        extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    );
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        #[link(name = $library, kind = "raw-dylib", modifiers = "+verbatim", import_name_type = "undecorated")]
        extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    )
}

/// Defines an external function to import.
#[cfg(all(windows, target_vendor = "win7", not(target_arch = "x86")))]
#[macro_export]
macro_rules! link {
    ("combase.dll" $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        #[link(name = "ole32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
        extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    );
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        #[link(name = $library, kind = "raw-dylib", modifiers = "+verbatim")]
        extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    )
}

// --- Non-Win7 Windows: verbatim upstream behavior. ---

/// Defines an external function to import.
#[cfg(all(windows, not(target_vendor = "win7"), target_arch = "x86"))]
#[macro_export]
macro_rules! link {
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        #[link(name = $library, kind = "raw-dylib", modifiers = "+verbatim", import_name_type = "undecorated")]
        extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    )
}

/// Defines an external function to import.
#[cfg(all(windows, not(target_vendor = "win7"), not(target_arch = "x86")))]
#[macro_export]
macro_rules! link {
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        #[link(name = $library, kind = "raw-dylib", modifiers = "+verbatim")]
        extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    )
}

/// Defines an external function to import.
#[cfg(not(windows))]
#[macro_export]
macro_rules! link {
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        extern $abi {
            pub fn $($function)*;
        }
    )
}
