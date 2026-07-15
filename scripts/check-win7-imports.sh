#!/usr/bin/env bash
# Windows 7 launchability gate.
#
# Two failure modes make a binary fail to start on Windows 7, and both are
# *load-time* — no runtime check can save them:
#
#   1. A post-Win7 function named as a static import in a DLL that DOES exist on
#      Win7 (e.g. user32!GetDpiForWindow). The loader can't resolve the symbol.
#   2. A dependency on a whole DLL that does NOT ship on Win7 (e.g. icuuc.dll,
#      bcryptprimitives.dll's ProcessPrng) — often dragged in by the Rust std for
#      the default Win10-floor MSVC targets. Use the tier-3 `*-win7-windows-msvc`
#      target to avoid these.
#
# This gate checks BOTH: it enumerates every dependent DLL and flags any not on a
# stock Win7 SP1 (+ Platform Update) machine, then scans the import table for known
# post-Win7 flat symbols. (COM classes created via CoCreateInstance are runtime-
# resolved and fail gracefully — not covered here by design.)
#
# Usage: scripts/check-win7-imports.sh [path-to-exe]
set -uo pipefail

EXE="${1:-target/debug/sample.exe}"

if [[ ! -f "$EXE" ]]; then
  echo "check-win7-imports: binary not found: $EXE" >&2
  exit 2
fi

DUMPBIN="$(command -v dumpbin.exe 2>/dev/null || true)"
if [[ -z "$DUMPBIN" ]]; then
  VSWHERE="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
  if [[ -f "$VSWHERE" ]]; then
    VSPATH="$("$VSWHERE" -latest -property installationPath | tr -d '\r')"
    DUMPBIN="$(find "$VSPATH/VC/Tools/MSVC" -name dumpbin.exe 2>/dev/null | head -1)"
  fi
fi
if [[ -z "$DUMPBIN" || ! -f "$DUMPBIN" ]]; then
  echo "check-win7-imports: dumpbin.exe not found (need MSVC build tools). Skipping." >&2
  exit 0
fi

FAIL=0

# ---------------------------------------------------------------------------
# Check 1 — dependent DLLs. Allowlist = DLLs present on a stock Win7 SP1 machine
# with the Platform Update (KB2670838). Anything else is a hard blocker.
# ---------------------------------------------------------------------------
# Core Win32 + the graphics/COM stack our app legitimately uses. Lowercased.
# (Whitespace is normalized to single spaces below so the membership test works.)
ALLOW='kernel32.dll user32.dll gdi32.dll shell32.dll ole32.dll oleaut32.dll
       advapi32.dll comdlg32.dll comctl32.dll imm32.dll usp10.dll ntdll.dll
       d2d1.dll dwrite.dll d3d11.dll dxgi.dll windowscodecs.dll uxtheme.dll
       dwmapi.dll shcore.dll shlwapi.dll version.dll winmm.dll rpcrt4.dll
       msimg32.dll setupapi.dll crypt32.dll ws2_32.dll'
ALLOW="$(echo "$ALLOW" | tr -s '[:space:]' ' ')"

# DLLs that do NOT exist on stock Win7 (Win8/Win10-only), with a hint each.
declare -A KNOWN_BAD=(
  [icuuc.dll]="ICU/Unicode — Win10-only. Use the tier-3 i686-win7-windows-msvc target."
  [icu.dll]="ICU/Unicode — Win10-only. Use the tier-3 win7 target."
  [icuin.dll]="ICU/Unicode — Win10-only. Use the tier-3 win7 target."
  [bcryptprimitives.dll]="ProcessPrng RNG — Win10 1809+. Comes from std on non-win7 targets."
  [combase.dll]="Win8+ COM base. Use the tier-3 win7 target (std falls back to ole32)."
  [api-ms-win-core-synch-l1-2-0.dll]="Win8+ sync API set (WaitOnAddress). tier-3 win7 target avoids it."
  [api-ms-win-core-winrt-error-l1-1-0.dll]="WinRT error API (RoOriginateErrorW) — Win8+. Comes from windows-result Error::new; avoid constructing Errors with messages."
  [api-ms-win-core-winrt-l1-1-0.dll]="WinRT core — Win8+."
  [api-ms-win-core-winrt-string-l1-1-0.dll]="WinRT string (HSTRING) — Win8+."
  [api-ms-win-shcore-scaling-l1-1-1.dll]="Per-monitor DPI scaling — Win8.1+."
)

DLLS="$("$DUMPBIN" //DEPENDENTS "$EXE" 2>/dev/null \
        | grep -iE '\.dll$' | tr -d '\r' | sed 's/^[[:space:]]*//' \
        | tr 'A-Z' 'a-z' | sort -u)"

echo "=== dependent DLLs ==="
while IFS= read -r dll; do
  [[ -z "$dll" ]] && continue
  if [[ -n "${KNOWN_BAD[$dll]:-}" ]]; then
    echo "  BLOCKER  $dll — ${KNOWN_BAD[$dll]}"
    FAIL=1
  elif [[ " $ALLOW " == *" $dll "* ]]; then
    echo "  ok       $dll"
  elif [[ "$dll" == vcruntime*.dll || "$dll" == msvcp*.dll ]]; then
    echo "  redist   $dll — VC++ runtime; install VC++ x86 redist OR build with crt-static."
  elif [[ "$dll" == api-ms-win-crt-*.dll ]]; then
    echo "  ucrt     $dll — Universal CRT; needs UCRT on Win7 (KB2999226) OR static CRT."
  elif [[ "$dll" == api-ms-win-*.dll ]]; then
    # Other API-set stubs default to BLOCKER: most Win8+ virtual DLLs live here, and
    # a soft pass is how api-ms-win-core-winrt-error slipped through once. Add a
    # verified-safe stub to the allowlist above rather than relaxing this default.
    echo "  BLOCKER  $dll — unrecognized API-set stub; likely Win8+. Add to allowlist only if verified on Win7."
    FAIL=1
  else
    echo "  UNKNOWN  $dll — not on the Win7 allowlist; verify it ships on Win7."
    FAIL=1
  fi
done <<< "$DLLS"

# ---------------------------------------------------------------------------
# Check 2 — post-Win7 flat imports from DLLs that DO exist on Win7.
# ---------------------------------------------------------------------------
BANNED='GetDpiForWindow|AdjustWindowRectExForDpi|GetDpiForSystem|GetDpiForMonitor|GetSystemMetricsForDpi|SystemParametersInfoForDpi|EnableNonClientDpiScaling|GetThreadDpiAwarenessContext|SetThreadDpiAwarenessContext|SetProcessDpiAwarenessContext|SetProcessDpiAwareness|PathCchCanonicalizeEx'
HITS="$("$DUMPBIN" //IMPORTS "$EXE" 2>/dev/null | grep -iE " ($BANNED)\$" \
        | sed 's/^[[:space:]]*[0-9a-fA-F]*[[:space:]]*/  /' | sort -u)"
echo "=== post-Win7 flat imports ==="
if [[ -n "$HITS" ]]; then
  echo "$HITS" | sed 's/^/  BLOCKER /'
  echo "  Route through a GetProcAddress shim (qt/src/sys.rs)."
  FAIL=1
else
  echo "  none"
fi

echo
if [[ "$FAIL" -ne 0 ]]; then
  echo "RESULT: FAIL — $EXE will not launch on a stock Windows 7." >&2
  exit 1
fi
echo "RESULT: OK — no Win7 load blockers in $EXE."
