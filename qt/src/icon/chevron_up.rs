use crate::icon::Icon;
use windows_core::s;

impl Icon {
    pub fn chevron_up_16_regular() -> Icon {
        Icon {
            svg: s!(
                r##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<path d="M3.15 10.35c.2.2.5.2.7 0L8 6.21l4.15 4.14a.5.5 0 0 0 .7-.7l-4.5-4.5a.5.5 0 0 0-.7 0l-4.5 4.5a.5.5 0 0 0 0 .7Z" fill="#212121"/>
</svg>"##
            ),
            size: 16,
        }
    }
}
