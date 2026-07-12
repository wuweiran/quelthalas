use windows::core::PCSTR;

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
pub mod font_decrease;
pub mod font_increase;
pub mod info;
pub mod more_horizontal;
pub mod select_all_on;
pub mod slide_text;
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
