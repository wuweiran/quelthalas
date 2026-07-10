//! `Item` — one entry in a selectable list. Shared by `dropdown` (pick-only) and
//! `combobox` (type-to-filter), mirroring Fluent's top-level `Option` (which lives
//! in `@fluentui/react-components`, not under any one component). Named `Item`
//! rather than `Option` to avoid clashing with `std::option::Option`.

use windows::core::PCWSTR;

/// One option in the list. `disabled` options are greyed, unclickable, and skipped
/// by keyboard navigation (Fluent's `<Option disabled>`).
#[derive(Copy, Clone)]
pub struct Item {
    pub text: PCWSTR,
    pub disabled: bool,
}

impl Item {
    pub fn new(text: PCWSTR) -> Self {
        Item {
            text,
            disabled: false,
        }
    }
    pub fn disabled(text: PCWSTR) -> Self {
        Item {
            text,
            disabled: true,
        }
    }
}
