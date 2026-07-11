# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.2.1]: https://github.com/wuweiran/quelthalas/releases/tag/v0.2.1
[0.2.0]: https://github.com/wuweiran/quelthalas/releases/tag/v0.2.0
[0.1.0]: https://github.com/wuweiran/quelthalas/releases/tag/v0.1.0
