use windows::core::w;
use windows::core::PCWSTR;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_SEMI_BOLD};

pub(crate) struct Tokens {
    pub color_neutral_background1: D2D1_COLOR_F,
    pub color_neutral_background1_hover: D2D1_COLOR_F,
    pub color_neutral_background1_pressed: D2D1_COLOR_F,
    pub color_brand_background: D2D1_COLOR_F,
    pub color_brand_background_hover: D2D1_COLOR_F,
    pub color_brand_background_pressed: D2D1_COLOR_F,
    pub color_neutral_foreground1: D2D1_COLOR_F,
    pub color_neutral_foreground1_hover: D2D1_COLOR_F,
    pub color_neutral_foreground1_pressed: D2D1_COLOR_F,
    pub color_neutral_foreground_on_brand: D2D1_COLOR_F,
    pub color_neutral_stroke1: D2D1_COLOR_F,
    pub color_neutral_stroke1_hover: D2D1_COLOR_F,
    pub color_neutral_stroke1_pressed: D2D1_COLOR_F,
    pub stroke_width_thin: f32,
    pub font_family_name: PCWSTR,
    pub font_weight_semibold: DWRITE_FONT_WEIGHT,
    pub font_size_base300: f32,
    pub spacing_horizontal_m: f32,
    pub border_radius_none: f32,
    pub border_radius_medium: f32,
    pub curve_easy_ease: [f64; 4],
    pub duration_faster: f64,
}

macro_rules! rgb {
    ($hex:expr) => {{
        const fn hex_char_to_u8(c: u8) -> u8 {
            match c {
                b'0'..=b'9' => (c as u8) - b'0',
                b'a'..=b'f' => (c as u8) - b'a' + 10,
                b'A'..=b'F' => (c as u8) - b'A' + 10,
                _ => panic!("Invalid hex digit"),
            }
        }

        let hex = $hex.as_bytes();
        let r = (hex_char_to_u8(hex[1]) * 16 + hex_char_to_u8(hex[2])) as f32 / 255.0;
        let g = (hex_char_to_u8(hex[3]) * 16 + hex_char_to_u8(hex[4])) as f32 / 255.0;
        let b = (hex_char_to_u8(hex[5]) * 16 + hex_char_to_u8(hex[6])) as f32 / 255.0;
        D2D1_COLOR_F { r, g, b, a: 1.0 }
    }};
}

impl Tokens {
    pub fn web_light() -> Self {
        Tokens {
            color_neutral_background1: rgb!("#ffffff"),
            color_neutral_background1_hover: rgb!("#f5f5f5"),
            color_neutral_background1_pressed: rgb!("#e0e0e0"),
            color_brand_background: rgb!("#0f6cbd"),
            color_brand_background_hover: rgb!("#115ea3"),
            color_brand_background_pressed: rgb!("#0c3b5e"),
            color_neutral_foreground1: rgb!("#242424"),
            color_neutral_foreground1_hover: rgb!("#242424"),
            color_neutral_foreground1_pressed: rgb!("#242424"),
            color_neutral_foreground_on_brand: rgb!("#ffffff"),
            color_neutral_stroke1: rgb!("#d1d1d1"),
            color_neutral_stroke1_hover: rgb!("#c7c7c7"),
            color_neutral_stroke1_pressed: rgb!("#b3b3b3"),
            stroke_width_thin: 1.0,
            font_family_name: w!("Segoe UI"),
            font_weight_semibold: DWRITE_FONT_WEIGHT_SEMI_BOLD,
            font_size_base300: 14f32,
            spacing_horizontal_m: 12f32,
            border_radius_none: 0f32,
            border_radius_medium: 4f32,
            curve_easy_ease: [0.33, 0.0, 0.67, 1.0],
            duration_faster: 0.1,
        }
    }
}
