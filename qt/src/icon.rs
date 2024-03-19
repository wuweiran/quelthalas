use windows::core::PCSTR;

pub mod calendar_month;

#[derive(Copy, Clone)]
pub struct Icon {
    pub(crate) svg: PCSTR,
}
