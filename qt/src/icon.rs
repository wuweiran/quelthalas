use windows::core::PCSTR;

pub mod calendar_month;
pub mod checkmark;
pub mod chevron_down;
pub mod chevron_right;
pub mod chevron_up;
pub mod slide_text;
pub mod triangle_down;
pub mod triangle_up;

#[derive(Copy, Clone)]
pub struct Icon {
    pub(crate) svg: PCSTR,
    pub(crate) size: usize,
}
