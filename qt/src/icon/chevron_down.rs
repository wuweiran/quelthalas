use crate::icon::Icon;
use windows_core::s;

impl Icon {
    pub fn chevron_down_20_regular() -> Icon {
        Icon {
            svg: s!(
                r##"<svg width="20" height="20" viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg">
<path d="M15.8537 7.64582C16.0493 7.84073 16.0499 8.15731 15.855 8.35292L10.39 13.8374C10.1751 14.0531 9.82574 14.0531 9.6108 13.8374L4.14582 8.35292C3.9509 8.15731 3.95147 7.84073 4.14708 7.64582C4.34269 7.4509 4.65927 7.45147 4.85418 7.64708L10.0004 12.8117L15.1466 7.64708C15.3415 7.45147 15.6581 7.4509 15.8537 7.64582Z" fill="#212121"/>
</svg>"##
            ),
            size: 20,
        }
    }

    pub fn chevron_down_16_regular() -> Icon {
        Icon {
            svg: s!(
                r##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
<path d="M3.15 5.65c.2-.2.5-.2.7 0L8 9.79l4.15-4.14a.5.5 0 0 1 .7.7l-4.5 4.5a.5.5 0 0 1-.7 0l-4.5-4.5a.5.5 0 0 1 0-.7Z" fill="#212121"/>
</svg>"##
            ),
            size: 16,
        }
    }
}
