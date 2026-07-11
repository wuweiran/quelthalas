use windows::core::PCSTR;

pub mod calendar_month;
pub mod checkmark;
pub mod checkmark_circle;
pub mod chevron_down;
pub mod chevron_right;
pub mod chevron_up;
pub mod diamond_dismiss;
pub mod info;
pub mod slide_text;
pub mod triangle_down;
pub mod triangle_up;
pub mod warning;

#[derive(Copy, Clone)]
pub struct Icon {
    pub(crate) svg: PCSTR,
    pub(crate) size: usize,
}
