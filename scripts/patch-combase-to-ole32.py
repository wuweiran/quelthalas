#!/usr/bin/env python3
"""Rewrite the `combase.dll` import to `ole32.dll` in a PE binary.

Why: the `windows-core` `#[implement]` machinery imports
`CoCreateFreeThreadedMarshaler` from `combase.dll` (via raw-dylib, so the DLL name
is hardcoded with no fallback). `combase.dll` is Windows 8+. On Windows 7 that same
function is exported by `ole32.dll`. The two names are import-table strings of equal
storable length (`combase.dll` = 11 bytes; `ole32.dll` = 9 bytes + 2 NUL pad = 11),
so we can rewrite the name in place without disturbing any offsets.

This is the lightweight alternative to vendoring windows-core with a one-line
`[patch]`. It edits only the ASCII DLL-name bytes; RVAs, thunks, and the IAT are
untouched (the loader resolves imports by name at load time).

Usage: patch-combase-to-ole32.py <path-to-exe>
Idempotent: a second run reports "already patched" and exits 0.
"""
import sys

OLD = b"combase.dll\x00"          # 12 bytes as stored (name + NUL)
NEW = b"ole32.dll\x00\x00\x00"    # 12 bytes: name + 3 NUL (same length)

def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path-to-exe>", file=sys.stderr)
        return 2
    path = sys.argv[1]
    with open(path, "rb") as f:
        data = bytearray(f.read())

    # Case-insensitive search for the import-name bytes. Import descriptor names are
    # stored verbatim as emitted by the linker (lowercase here), but match loosely.
    hay = bytes(data).lower()
    needle = OLD.lower()
    hits = []
    start = 0
    while True:
        i = hay.find(needle, start)
        if i < 0:
            break
        hits.append(i)
        start = i + 1

    if not hits:
        if bytes(data).lower().find(b"ole32.dll\x00") >= 0 and \
           bytes(data).lower().find(b"combase.dll\x00") < 0:
            print("already patched (combase.dll absent, ole32.dll present).")
            return 0
        print("ERROR: 'combase.dll\\0' not found — nothing to patch.", file=sys.stderr)
        return 1

    if len(hits) != 1:
        print(f"ERROR: expected exactly 1 'combase.dll' occurrence, found {len(hits)} "
              f"at {hits}. Aborting (could be a string other than the import name).",
              file=sys.stderr)
        return 1

    off = hits[0]
    data[off:off + len(NEW)] = NEW
    with open(path, "wb") as f:
        f.write(data)
    print(f"patched combase.dll -> ole32.dll at offset 0x{off:x} in {path}")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
