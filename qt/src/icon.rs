use windows::core::PCSTR;

pub mod calendar_month;
pub mod checkmark;
pub mod chevron_down;
pub mod chevron_right;
pub mod chevron_up;
pub mod slide_text;

#[derive(Copy, Clone)]
pub struct Icon {
    pub(crate) svg: PCSTR,
    pub(crate) size: usize,
}
