use windows::core::PCSTR;

pub mod arrow_down;
pub mod arrow_up;
pub mod calendar_month;
pub mod checkmark;
pub mod checkmark_circle;
pub mod chevron_down;
pub mod chevron_right;
pub mod chevron_up;
pub mod clipboard_paste;
pub mod copy;
pub mod cut;
pub mod diamond_dismiss;
pub mod dismiss;
pub mod font_decrease;
pub mod font_increase;
pub mod info;
pub mod more_horizontal;
pub mod presence_available;
pub mod presence_away;
pub mod presence_blocked;
pub mod presence_busy;
pub mod presence_dnd;
pub mod presence_offline;
pub mod presence_oof;
pub mod presence_unknown;
pub mod search;
pub mod select_all_on;
pub mod slide_text;
pub mod square;
pub mod text_bold;
pub mod text_bullet_list;
pub mod text_font;
pub mod text_italic;
pub mod text_number_list;
pub mod text_underline;
pub mod triangle_down;
pub mod triangle_up;
pub mod warning;

#[derive(Copy, Clone)]
pub struct Icon {
    pub(crate) svg: PCSTR,
    pub(crate) size: usize,
}
