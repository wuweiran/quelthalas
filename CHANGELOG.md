# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2026-07-16

### Added

- **Windows 7 support.** The library now runs on Windows 7 SP1. Icons render as
  Direct2D 1.0 path geometry (parsed from the Fluent SVG `d` strings) instead of
  Direct2D SVG documents; per-monitor DPI is resolved at runtime via
  `GetProcAddress` with a system-DPI fallback; and animations use the v1 Windows
  Animation Manager with a custom cubic-bezier interpolator that reproduces the
  Fluent easing curve exactly. A standard stable-Rust build still targets Windows
  10+ (the prebuilt `std` does); the tier-3 `*-win7-windows-msvc` target builds
  for Windows 7. See the README's "Windows 7" section.
- **`QT::scale_factor(window)`** — a public DPI scale accessor for callers sizing
  controls (routed through the DPI shim, so it is Windows 7-safe).

### Changed

- **BREAKING: control geometry is now physical device pixels.** `x`, `y`, `width`,
  and `height` on every component's `Props` are physical pixels (the Win32
  convention), so a DPI-aware caller scales its logical sizes by
  `QT::scale_factor(window)`. Previously `width`/`height` were treated as DIPs
  (auto-scaled) in most components but as physical pixels in a few — this unifies
  them. Callers passing fixed `width`/`height` must now multiply by the scale
  factor to keep the same apparent size.
- **TreeView**: row labels get a small left padding (`spacingHorizontalXXS`).

### Fixed

- **Dropdown**: clicking the field while the list is open now closes it (and stays
  closed) instead of immediately re-opening, matching the combobox toggle.

## [0.3.0] - 2026-07-13

### Added

- **DataGrid** — a multi-column `SysListView32`-style grid with sortable columns:
  click a header to sort, with an up/down arrow indicating the direction.
- **Calendar** — a Fluent month/day view modeled on the Win32 `SysMonthCal32`
  month calendar.
- **DatePicker** — a read-only date field with a Calendar flyout, formatting the
  selected date with the OS short-date pattern (`GetDateFormatEx`).
- **SearchBox** — a Fluent search input with a leading search glyph and a
  trailing dismiss button.
- **MessageBar** — an inline notification strip in all four intents (info /
  success / warning / error), with a semibold title, a message, and optional
  trailing action buttons hosted as real Button children.
- **Toolbar** — a Fluent command bar (used by the sample's rich-text editor
  toolbar) with icon buttons.
- **Avatar** — a circular initials avatar whose color is derived from the name
  via Fluent's exact name-hash, across the 24 / 32 / 48 / 72 size ramp, with an
  optional corner PresenceBadge.
- **PresenceBadge** — a standalone status badge (available / away / busy /
  do-not-disturb / blocked / offline / out-of-office / unknown) drawn with
  Fluent's native-size presence glyphs (10 / 12 / 16 / 20 px).
- **Image** — a framed picture surface decoded through WIC, with `fit` modes
  (none / center / default / contain / cover), a `shape` clip (square / rounded /
  circular), and an optional border.

### Changed

- **Menu**: dropdown items can now show a leading icon, gained a pressed visual,
  and deselect when the mouse leaves the item.

### Fixed

- **Button**: corrected the Small appearance's minimum width (Small 64 / Medium
  96 / Large 96, matching Fluent).

## [0.2.1] - 2026-07-11

### Fixed

- **Edit context menu**: the Cut / Copy / Paste / Select All labels are now
  correctly localized. They live in a user32 MENU resource (not a string table),
  so the previous `LoadStringW` lookups always fell back to English; they're now
  read from that menu and match the current Windows UI language.
- **Localization**: strip the whole Alt-mnemonic from system labels, including the
  parenthesized CJK form (`剪切(&T)` → `剪切`), instead of only the `&`.
- **SplitButton**: clicking the chevron while its dropdown is open now closes the
  menu instead of immediately re-opening it.

### Changed

- Resolve system labels (edit-menu commands and dialog buttons) into caller-owned
  buffers per use, removing the previous per-call leak.

## [0.2.0] - 2026-07-11

### Added

- **ListBox** — a single-select Win32 list with a Fluent brand accent bar and a
  press-shrink selection animation, built on the shared `scroll` helper.
- **TreeView** — a lazily-loaded hierarchy with animated chevron twisties and an
  `on_expand` callback per node.
- **SplitButton** — a `BS_SPLITBUTTON`-style control with an independent action
  zone and dropdown zone (per-zone hover/press), a full-height divider, and a
  right-aligned dropdown menu. Available in Default and Primary appearances.
- **Divider** — a Fluent horizontal rule with an optional aligned label and
  Default / Subtle / Brand / Strong appearances driving both the line and label
  colors.
- **TaskDialog** — a Win32-style task dialog with a two-tier text hierarchy, an
  intent icon (Info / Warning / Error / Success), common command buttons, custom
  command links, and a verification checkbox.
- **Dialog**: `Actions` selector so a dialog can show a single "OK" button
  (`Actions::Ok`) instead of the "OK"/"Cancel" pair (`Actions::OkCancel`).
- **Scroll helper**: press-and-hold auto-repeat on the arrow buttons and the
  track, matching the WinUI 3 desktop scrollbar.

### Changed

- Standard dialog and task-dialog buttons (OK / Cancel / Yes / No / Retry /
  Close) are now localized from the system string table, matching the current
  Windows UI language — the same mechanism the edit context menu uses.

### Fixed

- **Menu**: right-aligned dropdowns (and the screen-edge clamps and submenu
  anchoring) now position correctly at non-100% display scaling.
- **Dialog**: correct centering over the owner window.
- **Textarea**: the caret no longer misbehaves over the scrollbar region.
- **TaskDialog**: eliminated command-link hover flicker, and the hover state now
  clears when the cursor leaves the window quickly.

## [0.1.0]

Initial release: Fluent UI 2-styled reimplementations of classic Win32 controls
drawn with Direct2D/DirectWrite — button, checkbox, combobox, dialog, dropdown,
input, link, menu, menu_bar, option, progress_bar, radio, slider, spin_button,
spinner, switch, tab_list, text, textarea, and tooltip, plus a shared `scroll`
helper.

[0.4.0]: https://github.com/wuweiran/quelthalas/releases/tag/v0.4.0
[0.3.0]: https://github.com/wuweiran/quelthalas/releases/tag/v0.3.0
[0.2.1]: https://github.com/wuweiran/quelthalas/releases/tag/v0.2.1
[0.2.0]: https://github.com/wuweiran/quelthalas/releases/tag/v0.2.0
[0.1.0]: https://github.com/wuweiran/quelthalas/releases/tag/v0.1.0
