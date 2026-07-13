use crate::icon::Icon;
use windows_core::s;

impl Icon {
    pub fn presence_dnd_10_filled() -> Icon {
        Icon {
            svg: s!(
                r##"<svg width="10" height="10" viewBox="0 0 10 10" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M5 10C7.76142 10 10 7.76142 10 5C10 2.23858 7.76142 0 5 0C2.23858 0 0 2.23858 0 5C0 7.76142 2.23858 10 5 10ZM3.5 4.5H6.5C6.77614 4.5 7 4.72386 7 5C7 5.27614 6.77614 5.5 6.5 5.5H3.5C3.22386 5.5 3 5.27614 3 5C3 4.72386 3.22386 4.5 3.5 4.5Z" fill="#212121"/></svg>"##
            ),
            size: 10,
        }
    }
    pub fn presence_dnd_12_filled() -> Icon {
        Icon {
            svg: s!(
                r##"<svg width="12" height="12" viewBox="0 0 12 12" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M6 12C9.31371 12 12 9.31371 12 6C12 2.68629 9.31371 0 6 0C2.68629 0 0 2.68629 0 6C0 9.31371 2.68629 12 6 12ZM3.75 5.25H8.25C8.66421 5.25 9 5.58579 9 6C9 6.41421 8.66421 6.75 8.25 6.75H3.75C3.33579 6.75 3 6.41421 3 6C3 5.58579 3.33579 5.25 3.75 5.25Z" fill="#212121"/></svg>"##
            ),
            size: 12,
        }
    }
    pub fn presence_dnd_16_filled() -> Icon {
        Icon {
            svg: s!(
                r##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M8 16C12.4183 16 16 12.4183 16 8C16 3.58172 12.4183 0 8 0C3.58172 0 0 3.58172 0 8C0 12.4183 3.58172 16 8 16ZM5.24902 7H10.7499C11.3022 7 11.7499 7.44772 11.7499 8C11.7499 8.55229 11.3022 9 10.7499 9H5.24902C4.69674 9 4.24902 8.55229 4.24902 8C4.24902 7.44772 4.69674 7 5.24902 7Z" fill="#212121"/></svg>"##
            ),
            size: 16,
        }
    }
    pub fn presence_dnd_20_filled() -> Icon {
        Icon {
            svg: s!(
                r##"<svg width="20" height="20" viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M10 20C15.5228 20 20 15.5228 20 10C20 4.47715 15.5228 0 10 0C4.47715 0 0 4.47715 0 10C0 15.5228 4.47715 20 10 20ZM7 9H13C13.5523 9 14 9.44771 14 10C14 10.5523 13.5523 11 13 11H7C6.44772 11 6 10.5523 6 10C6 9.44771 6.44772 9 7 9Z" fill="#212121"/></svg>"##
            ),
            size: 20,
        }
    }
}
