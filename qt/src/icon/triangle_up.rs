use crate::icon::Icon;
use windows_core::s;

impl Icon {
    pub fn triangle_up_20_filled() -> Icon {
        Icon {
            // PLACEHOLDER — replace the <svg>…</svg> below with the contents of
            // https://raw.githubusercontent.com/microsoft/fluentui-system-icons/main/assets/Triangle%20Up/SVG/ic_fluent_triangle_up_20_filled.svg
            svg: s!(
                r##"<svg width="20" height="20" viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg">
<path d="M11.3195 2.78548C10.7522 1.73807 9.24903 1.73807 8.68166 2.78548L2.1822 14.7841C1.64081 15.7835 2.36446 16.9985 3.50113 16.9985H16.5C17.6367 16.9985 18.3604 15.7835 17.819 14.7841L11.3195 2.78548Z" fill="#212121"/>
</svg>"##
            ),
            size: 20,
        }
    }
}
