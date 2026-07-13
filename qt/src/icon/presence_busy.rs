use crate::icon::Icon;
use windows_core::s;

impl Icon {
    pub fn presence_busy_10_filled() -> Icon {
        Icon {
            svg: s!(
                r##"<svg width="10" height="10" viewBox="0 0 10 10" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M10 5C10 7.76142 7.76142 10 5 10C2.23858 10 0 7.76142 0 5C0 2.23858 2.23858 0 5 0C7.76142 0 10 2.23858 10 5Z" fill="#212121"/></svg>"##
            ),
            size: 10,
        }
    }
    pub fn presence_busy_12_filled() -> Icon {
        Icon {
            svg: s!(
                r##"<svg width="12" height="12" viewBox="0 0 12 12" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M12 6C12 9.31371 9.31371 12 6 12C2.68629 12 0 9.31371 0 6C0 2.68629 2.68629 0 6 0C9.31371 0 12 2.68629 12 6Z" fill="#212121"/></svg>"##
            ),
            size: 12,
        }
    }
    pub fn presence_busy_16_filled() -> Icon {
        Icon {
            svg: s!(
                r##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M16 8C16 12.4183 12.4183 16 8 16C3.58172 16 0 12.4183 0 8C0 3.58172 3.58172 0 8 0C12.4183 0 16 3.58172 16 8Z" fill="#212121"/></svg>"##
            ),
            size: 16,
        }
    }
    pub fn presence_busy_20_filled() -> Icon {
        Icon {
            svg: s!(
                r##"<svg width="20" height="20" viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M20 10C20 15.5228 15.5228 20 10 20C4.47715 20 0 15.5228 0 10C0 4.47715 4.47715 0 10 0C15.5228 0 20 4.47715 20 10Z" fill="#212121"/></svg>"##
            ),
            size: 20,
        }
    }
}
